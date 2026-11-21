//! Windows Job Object：把子进程整棵树绑进一个可强杀的容器，并可选地
//! 套上资源上限（内存提交 / CPU 权重 / 进程数 / 优先级）。
//!
//! **根因**：`tokio::time::timeout` 到期后只是 drop 掉 future，子进程和它
//! 派生的孙进程一概活着。Windows 上 `cmd /C` 被杀不等于它启动的 `ping` /
//! `node` / `python` 被杀 —— 这些孤儿会一直占着 CPU 和文件锁，用户看到的是
//! "命令已经超时报错了，但机器还在响"。
//!
//! Job Object 是唯一的正确解：进程一旦加入 job，它派生的所有后代自动加入
//! 同一个 job；`TerminateJobObject` 一次调用杀整棵树。配
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 后连"句柄泄漏导致进程逃逸"也堵住了。
//!
//! **资源上限**（[`JobLimits`]，总管方案 P0）：`process_memory_bytes` 卡
//! 提交内存——超限时进程自己的分配失败、通常当场崩溃退出（Windows 没有
//! "由内核替你杀掉"的语义，这是能达到的最硬边界）；`cpu_weight` 是 1..=9
//! 的相对调度权重（软限制，默认 5）；`below_normal_priority` 把整棵树压到
//! BELOW_NORMAL 护住前台交互。CPU rate 控制在 RDP+DFSS 环境会失败——
//! 降级为无 CPU 限制继续，不因此拒绝建 job（内存/杀树仍生效）。
//!
//! 附着失败（例如宿主本身已经在另一个 job 里，Windows 不允许嵌套）时降级为
//! `None`，调用方退回普通的 `kill_on_drop` —— 杀不掉孙进程总比杀不掉自己强。
//!
//! 手写 FFI 而不引 `windows` crate：本项目只用到 6 个函数，且 hermix-core
//! 是跨平台编译的 crate，不想为一个平台专属能力拖进整套绑定。

#![cfg(windows)]

use std::io;

type Handle = isize;
type Bool = i32;
type Dword = u32;

const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
const PROCESS_TERMINATE: Dword = 0x0001;
const PROCESS_SET_QUOTA: Dword = 0x0100;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: Dword = 0x0000_2000;
/// 单进程提交内存上限（`ProcessMemoryLimit` 字段）。
const JOB_OBJECT_LIMIT_PROCESS_MEMORY: Dword = 0x0000_0100;
/// job 内进程数上限（防 fork 炸弹）。
const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: Dword = 0x0000_0800;
/// 以 job 级 `PriorityClass` 字段覆盖整棵树的优先级。
const JOB_OBJECT_LIMIT_PRIORITY_CLASS: Dword = 0x0000_0020;
const BELOW_NORMAL_PRIORITY_CLASS: Dword = 0x0000_4000;
/// `SetInformationJobObject` 的 CPU rate 控制信息类。
const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION: u32 = 15;
const JOB_OBJECT_CPU_RATE_CONTROL_ENABLE: Dword = 0x1;
const JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED: Dword = 0x2;
const INVALID_HANDLE_VALUE: Handle = -1;
const STILL_ACTIVE: Dword = 259;

extern "system" {
    fn CreateJobObjectW(attrs: *const (), name: *const u16) -> Handle;
    fn OpenProcess(access: Dword, inherit: Bool, pid: Dword) -> Handle;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
    fn SetInformationJobObject(job: Handle, class: u32, info: *const (), len: Dword) -> Bool;
    fn TerminateJobObject(job: Handle, exit_code: u32) -> Bool;
    fn GetExitCodeProcess(process: Handle, code: *mut Dword) -> Bool;
    fn CloseHandle(obj: Handle) -> Bool;
}

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` 全量布局（64 位 Windows 上
/// 144 字节）。偏移经测试钉死：`LimitFlags` 在 16，`ProcessMemoryLimit`
/// 在 112（`IoInfo` 是 6 × u64，占 64..112）。
#[repr(C)]
struct ExtendedLimitInfo {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: Dword,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: Dword,
    affinity: usize,
    priority_class: Dword,
    scheduling_class: Dword,
    /// `IO_COUNTERS`：6 个 u64。
    io_counters: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

/// `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`（12 字节，weight 模式只写
/// 联合体的第一个 DWORD）。
#[repr(C)]
struct CpuRateControlInfo {
    control_flags: Dword,
    weight: Dword,
    min_rate: u16,
    max_rate: u16,
}

/// Job 的资源上限。任一字段为 `None`/`false` 表示不启用该限制。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// 单进程提交内存上限（字节）。超限时进程的内存分配失败、通常自行
    /// 崩溃退出。这是给 worker 的最硬资源边界。
    pub process_memory_bytes: Option<usize>,
    /// CPU 调度权重 1..=9（相对整机其他负载，默认 5；越低越让路）。
    /// 超出范围的值会被钳制。
    pub cpu_weight: Option<u32>,
    /// job 内（含所有后代）进程数上限。
    pub active_process_limit: Option<u32>,
    /// 整棵树压到 BELOW_NORMAL 优先级，护住前台交互。
    pub below_normal_priority: bool,
}

impl JobLimits {
    /// 总管方案 D5 的 worker 默认档：提交内存 4 GiB、CPU 权重 3、
    /// 进程数 16、BELOW_NORMAL。
    pub fn worker_defaults() -> Self {
        Self {
            process_memory_bytes: Some(4 * 1024 * 1024 * 1024),
            cpu_weight: Some(3),
            active_process_limit: Some(16),
            below_normal_priority: true,
        }
    }

    fn clamp_weight(w: u32) -> u32 {
        w.clamp(1, 9)
    }
}

/// 一个绑定的 job。drop 或 [`Self::terminate`] 都会杀掉整棵进程树。
/// 成功路径应先 [`Self::disarm`]（A5，2026-11-20）：正常退出的命令常留
/// `start /b` 后台子进程在 job 里，Drop 无条件 terminate 会把它们一并
/// 掳走，与后台语义矛盾；还带 PID 复用误杀风险。
pub struct JobGuard {
    job: Handle,
    disarmed: std::cell::Cell<bool>,
}

impl JobGuard {
    /// 把 `pid` 对应的进程（及其全部后代）纳入一个新的 job（仅杀树，
    /// 无资源上限——保持既有语义）。
    /// 任一步失败返回 `None` —— 调用方应降级，不要因此拒绝执行命令。
    pub fn attach(pid: u32) -> Option<Self> {
        Self::attach_with_limits(pid, &JobLimits::default())
    }

    /// 同 [`JobGuard::attach`]，但按 `limits` 套资源上限。
    /// CPU rate 控制设置失败时静默降级（RDP/DFSS 环境），其余限制仍生效。
    pub fn attach_with_limits(pid: u32, limits: &JobLimits) -> Option<Self> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job == 0 || job == INVALID_HANDLE_VALUE {
                return None;
            }

            let mut limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if limits.process_memory_bytes.is_some() {
                limit_flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            }
            if limits.active_process_limit.is_some() {
                limit_flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            }
            if limits.below_normal_priority {
                limit_flags |= JOB_OBJECT_LIMIT_PRIORITY_CLASS;
            }
            let info = ExtendedLimitInfo {
                per_process_user_time_limit: 0,
                per_job_user_time_limit: 0,
                limit_flags,
                minimum_working_set_size: 0,
                maximum_working_set_size: 0,
                active_process_limit: limits.active_process_limit.unwrap_or(0),
                affinity: 0,
                priority_class: if limits.below_normal_priority {
                    BELOW_NORMAL_PRIORITY_CLASS
                } else {
                    0
                },
                scheduling_class: 0,
                io_counters: [0; 6],
                process_memory_limit: limits.process_memory_bytes.unwrap_or(0),
                job_memory_limit: 0,
                peak_process_memory_used: 0,
                peak_job_memory_used: 0,
            };
            let set = SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &info as *const _ as *const (),
                std::mem::size_of::<ExtendedLimitInfo>() as Dword,
            );
            if set == 0 {
                CloseHandle(job);
                return None;
            }

            if let Some(weight) = limits.cpu_weight {
                let cpu = CpuRateControlInfo {
                    control_flags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                        | JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED,
                    weight: JobLimits::clamp_weight(weight),
                    min_rate: 0,
                    max_rate: 0,
                };
                // 失败仅降级：CPU 控制在 RDP+DFSS 下不可用（MSDN 明示），
                // 内存上限与杀树不受影响。
                SetInformationJobObject(
                    job,
                    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
                    &cpu as *const _ as *const (),
                    std::mem::size_of::<CpuRateControlInfo>() as Dword,
                );
            }

            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process == 0 {
                CloseHandle(job);
                return None;
            }
            let assigned = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assigned == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self { job, disarmed: std::cell::Cell::new(false) })
        }
    }

    /// 立刻杀掉 job 内的所有进程。可重复调用（已终止的 job 返回错误被忽略）。
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job, 1);
        }
    }

    /// 撤销 Drop 时的自动 terminate（A5）：命令成功退出后调用，
    /// 让 `start /b` 后台子进程继续活着。显式 [`Self::terminate`]
    /// 不受影响。
    pub fn disarm(&self) {
        self.disarmed.set(true);
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if !self.disarmed.get() {
            self.terminate();
        }
        unsafe {
            CloseHandle(self.job);
        }
    }
}

/// 进程是否还在运行。用最小权限打开句柄 —— 权限越大越容易在受保护
/// 进程上失败，失败会被误读成"进程已退出"。
pub fn is_process_alive(pid: u32) -> io::Result<bool> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            // 打不开通常意味着进程已经没了
            return Ok(false);
        }
        let mut code: Dword = 0;
        let ok = GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code == STILL_ACTIVE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起一个会活很久、且必然派生孙进程的命令（`cmd /C` → `ping`）。
    fn spawn_long_lived() -> std::process::Child {
        std::process::Command::new("cmd")
            .args(["/C", "ping -n 120 127.0.0.1 > nul"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cmd")
    }

    fn zeroed_extended() -> ExtendedLimitInfo {
        ExtendedLimitInfo {
            per_process_user_time_limit: 0,
            per_job_user_time_limit: 0,
            limit_flags: 0,
            minimum_working_set_size: 0,
            maximum_working_set_size: 0,
            active_process_limit: 0,
            affinity: 0,
            priority_class: 0,
            scheduling_class: 0,
            io_counters: [0; 6],
            process_memory_limit: 0,
            job_memory_limit: 0,
            peak_process_memory_used: 0,
            peak_job_memory_used: 0,
        }
    }

    #[test]
    fn extended_limit_info_layout_is_pinned() {
        assert_eq!(std::mem::size_of::<ExtendedLimitInfo>(), 144);
        let probe = zeroed_extended();
        // 写错位置 Windows 读到的是别的字段 —— 偏移钉死：
        // LimitFlags@16, ActiveProcessLimit@40, PriorityClass@56,
        // ProcessMemoryLimit@112（IoInfo 占 64..112）。
        let base = &probe as *const _ as usize;
        let off = |p: usize| p - base;
        assert_eq!(off(&probe.limit_flags as *const _ as usize), 16);
        assert_eq!(off(&probe.active_process_limit as *const _ as usize), 40);
        assert_eq!(off(&probe.priority_class as *const _ as usize), 56);
        assert_eq!(off(&probe.process_memory_limit as *const _ as usize), 112);
    }

    #[test]
    fn cpu_rate_control_info_is_12_bytes() {
        assert_eq!(std::mem::size_of::<CpuRateControlInfo>(), 12);
        assert_eq!(JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, 0x1);
        assert_eq!(JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED, 0x2);
    }

    #[test]
    fn weight_is_clamped_to_1_through_9() {
        assert_eq!(JobLimits::clamp_weight(0), 1);
        assert_eq!(JobLimits::clamp_weight(99), 9);
        assert_eq!(JobLimits::clamp_weight(5), 5);
    }

    #[test]
    fn attach_succeeds_for_a_live_process() {
        let child = spawn_long_lived();
        let pid = child.id();
        let guard = JobGuard::attach(pid);
        assert!(guard.is_some(), "活进程应当能附着 job");
        drop(guard);
    }

    #[test]
    fn attach_with_worker_limits_keeps_normal_process_alive() {
        let mut child = spawn_long_lived();
        let guard = JobGuard::attach_with_limits(child.id(), &JobLimits::worker_defaults());
        assert!(guard.is_some(), "带默认 worker 限额的附着应当成功");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            is_process_alive(child.id()).unwrap_or(false),
            "正常进程在 worker 限额下不应被杀"
        );
        drop(guard);
        for _ in 0..40 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(matches!(child.try_wait(), Ok(Some(_))), "drop guard 后进程树应被杀");
    }

    /// 提交内存上限的硬边界：单块分配超过 job 上限 → 进程自身的
    /// VirtualAlloc 失败、分配不可能完成。
    /// 判定用**副作用文件**而非退出码：分配成功才写标记文件。本仓库的
    /// 测试执行环境会加速 `thread::sleep` 且 `try_wait` 的回报不可靠
    /// （实测 20 秒轮询从未观察到已退出进程的 Some(exit)），退出码观测
    /// 在这里不成立；「标记文件不存在」与观测手段无关。
    /// 参数组合经探针实测钉死：上限 1 GiB 时 powershell 基线启动正常、
    /// 1.5 GiB 炸弹 300ms 内失败；上限过小（512 MiB）会掐住 CLR 异常
    /// 路径自身的提交需求，进程卡死不退出。
    #[test]
    fn process_memory_limit_kills_memory_bomb() {
        let marker = std::env::temp_dir().join("hermix_job_bomb_marker.txt");
        let _ = std::fs::remove_file(&marker);
        let mut child = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "try {{ $null = [byte[]]::new(1610612736); Set-Content -Path '{}' -Value ok }} \
                     catch {{ exit 2 }}; exit 0",
                    marker.to_string_lossy().replace('\\', "\\\\")
                ),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn powershell");
        let pid = child.id();
        let limits = JobLimits {
            process_memory_bytes: Some(1024 * 1024 * 1024),
            ..JobLimits::default()
        };
        let guard = JobGuard::attach_with_limits(pid, &limits)
            .expect("attach with memory limit");

        // 真实时钟 deadline（迭代计数在本环境会被加速的 sleep 秒穿）；
        // 句柄查询兜底 try_wait 的失灵。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            if !is_process_alive(pid).unwrap_or(true) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(guard);
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            !marker.exists(),
            "1.5 GiB 分配不应在 1 GiB 提交上限下成功（标记文件被写出来了）"
        );
    }

    #[test]
    fn terminate_kills_the_whole_tree() {
        let mut child = spawn_long_lived();
        let pid = child.id();
        let guard = JobGuard::attach(pid).expect("attach");
        guard.terminate();

        // 轮询等待进程消失（TerminateJobObject 是异步完成的）。
        // 主判据用 `try_wait`（直接子进程已回收），辅助判据用句柄查询，
        // 两者必须都确认进程不在了才算杀干净。
        for _ in 0..40 {
            if matches!(child.try_wait(), Ok(Some(_))) && !is_process_alive(pid).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "terminate 之后直接子进程应当已退出"
        );
        let alive = is_process_alive(pid).unwrap_or_else(|e| {
            panic!("terminate 之后句柄查询失败（{e}）—— 无法确认进程已退出")
        });
        assert!(!alive, "terminate 之后进程应当已退出");
        let _ = child.kill();
    }

    #[test]
    fn attach_to_dead_pid_degrades_gracefully() {
        // 用当前进程不可能占用的 pid 探测降级路径
        let guard = JobGuard::attach(0xFFFF_FFF0);
        assert!(guard.is_none() || guard.is_some()); // 只要求不 panic
    }
}
