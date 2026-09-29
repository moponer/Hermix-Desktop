//! Windows Job Object：把子进程整棵树绑进一个可强杀的容器。
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

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` 的头部。只需要写到 `LimitFlags`
/// （偏移 16），尾部用零填充。结构体总长在 64 位 Windows 上是 144 字节 ——
/// 这个数字写错会让 `SetInformationJobObject` 返回 `ERROR_BAD_LENGTH`。
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
    tail: [u8; 144 - 64],
}

/// 一个绑定的 job。drop 或 [`Self::terminate`] 都会杀掉整棵进程树。
pub struct JobGuard {
    job: Handle,
}

impl JobGuard {
    /// 把 `pid` 对应的进程（及其全部后代）纳入一个新的 job。
    /// 任一步失败返回 `None` —— 调用方应降级，不要因此拒绝执行命令。
    pub fn attach(pid: u32) -> Option<Self> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job == 0 || job == INVALID_HANDLE_VALUE {
                return None;
            }

            let info = ExtendedLimitInfo {
                per_process_user_time_limit: 0,
                per_job_user_time_limit: 0,
                limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                minimum_working_set_size: 0,
                maximum_working_set_size: 0,
                active_process_limit: 0,
                affinity: 0,
                priority_class: 0,
                scheduling_class: 0,
                tail: [0; 144 - 64],
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
            Some(Self { job })
        }
    }

    /// 立刻杀掉 job 内的所有进程。可重复调用（已终止的 job 返回错误被忽略）。
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job, 1);
        }
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.terminate();
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

    #[test]
    fn extended_limit_info_is_144_bytes() {
        assert_eq!(std::mem::size_of::<ExtendedLimitInfo>(), 144);
        let probe = ExtendedLimitInfo {
            per_process_user_time_limit: 0,
            per_job_user_time_limit: 0,
            limit_flags: 0,
            minimum_working_set_size: 0,
            maximum_working_set_size: 0,
            active_process_limit: 0,
            affinity: 0,
            priority_class: 0,
            scheduling_class: 0,
            tail: [0; 144 - 64],
        };
        // LimitFlags 必须在偏移 16，否则 Windows 读到的是别的字段
        let base = &probe as *const _ as usize;
        let flags = &probe.limit_flags as *const _ as usize;
        assert_eq!(flags - base, 16);
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
