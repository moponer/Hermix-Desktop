//! 系统资源管理 —— 进程快照 / 详情 / 结束，磁盘容量。
//!
//! 算法参考 System Informer（原 Process Hacker，MIT）：
//! - 进程列表用**一次内核快照**（`SystemProcessInformation`）而不是逐进程
//!   `OpenProcess`。这里用 `sysinfo` 承载该算法：它内部就是一次全量快照 +
//!   两次采样差分算 CPU，比自己解析 `SYSTEM_PROCESS_INFORMATION` 稳（该结构
//!   随 Windows 版本变长，手写解析很容易在某个版本上越界）。
//! - 父子关系靠 `InheritedFromUniqueProcessId`，UI 侧建树。
//! - CPU 占用是**差分**：同一进程两次采样的 CPU 时间差 / 墙钟差，首次采样
//!   没有基准，返回 0（System Informer 同样是"第二个周期才有数"）。
//! - 描述/公司取 exe 的版本信息（`VerQueryValue`，与 System Informer 的
//!   `PhGetFileVersionInfo` 同源），不是猜的。

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::Serialize;

/// 两次采样之间的最小间隔：CPU 差分太密会把噪声放大成 100%。
const CPU_SAMPLE_GAP: Duration = Duration::from_millis(300);

/// 关键进程：结束它们会直接蓝屏/注销/断网，界面层也一并禁用。
/// 名单参考 System Informer 的 "critical process" 判定 + 微软文档的
/// "Cannot be terminated" 列表，**只放绝对不该杀的**，其余交给系统返回
/// 拒绝访问（不越权替用户做判断）。
const CRITICAL_NAMES: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "fontdrvhost.exe",
    "dwm.exe",
];

struct Sampler {
    system: sysinfo::System,
    sampled_at: Instant,
}

fn sampler() -> &'static Mutex<Sampler> {
    static SAMPLER: OnceLock<Mutex<Sampler>> = OnceLock::new();
    SAMPLER.get_or_init(|| {
        Mutex::new(Sampler {
            // `new_all` 一次性把 CPU 列表与内存总量灌进去；之后只做
            // `refresh_processes` / `refresh_memory` 的增量刷新。
            system: sysinfo::System::new_all(),
            sampled_at: Instant::now() - Duration::from_secs(10),
        })
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    /// exe 完整路径（无权限时为 None——系统进程常见）。
    pub exe: Option<String>,
    /// 命令行（无权限时退回 exe 路径）。
    pub cmd: String,
    /// 相对单核的百分比；首次采样为 0。
    pub cpu: f32,
    pub memory_mb: f64,
    pub user: Option<String>,
    pub status: String,
    /// Unix 秒。
    pub started_at: u64,
    /// exe 的 FileDescription，例如「Windows 资源管理器」。
    pub description: Option<String>,
    pub company: Option<String>,
    /// 关键进程：UI 禁用结束按钮（服务端仍会二次拒绝）。
    pub critical: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemOverview {
    pub total_memory_mb: f64,
    pub used_memory_mb: f64,
    pub cpu_count: usize,
    pub process_count: usize,
    /// 全局 CPU 占用（0–100，相对全部逻辑核心；同样来自两次采样差分）。
    pub cpu_usage: f32,
    /// 开机时长（秒），前端显示为"已运行 x 天 y 小时"。
    pub uptime_secs: u64,
    pub host_name: String,
    pub os_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub processes: Vec<ProcessRow>,
    pub overview: SystemOverview,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskRow {
    pub name: String,
    pub mount: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub removable: bool,
    /// 卷标存在时的盘符（`C:`），扫描/清理用它做过滤键。
    pub drive: String,
}

// ── Tauri 命令 ────────────────────────────────────────────────────────
// 快照会阻塞 ~300ms 做 CPU 差分采样，一律走 blocking 线程池，
// 不占 Tauri 的 IPC/主线程（主线程卡住会连窗口拖动一起卡）。

#[tauri::command]
pub async fn sysmon_processes() -> Result<ProcessSnapshot, String> {
    tauri::async_runtime::spawn_blocking(snapshot)
        .await
        .map_err(|error| format!("进程快照失败：{error}"))
}

#[tauri::command]
pub async fn sysmon_kill_process(pid: u32, force: Option<bool>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || kill_process(pid, force.unwrap_or(true)))
        .await
        .map_err(|error| format!("结束进程失败：{error}"))?
}

#[tauri::command]
pub async fn sysmon_disks() -> Result<Vec<DiskRow>, String> {
    tauri::async_runtime::spawn_blocking(disks)
        .await
        .map_err(|error| format!("读取磁盘信息失败：{error}"))
}

/// 采一次快照并返回进程列表。
///
/// 阻塞 ~300ms（CPU 差分需要两次采样），命令层用 `spawn_blocking` 调，
/// 不占 Tauri 的 IPC 线程。
pub fn snapshot() -> ProcessSnapshot {
    let mut guard = match sampler().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let since = guard.sampled_at.elapsed();
    if since < CPU_SAMPLE_GAP {
        // 距上次采样太近：补足间隔，否则差分被放大成离谱的百分比。
        std::thread::sleep(CPU_SAMPLE_GAP - since);
    }
    guard
        .system
        .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    // 内存占用是"当前值"，每次都要刷；`refresh_memory` 只读几个计数器。
    guard.system.refresh_memory();
    // 这一次 refresh 与上一次之间形成了差分窗口 → cpu_usage 才有意义。
    guard.sampled_at = Instant::now();

    let system = &guard.system;
    let mut users: Option<sysinfo::Users> = None;
    let mut rows: Vec<ProcessRow> = Vec::with_capacity(system.processes().len());

    for (pid, process) in system.processes() {
        let raw_name = process.name().to_string_lossy().to_string();
        let lower = raw_name.to_ascii_lowercase();
        let critical = pid.as_u32() <= 4 || CRITICAL_NAMES.iter().any(|n| lower == *n);

        let exe = process.exe().map(|path| path.display().to_string());
        let cmd = {
            let parts: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            if parts.is_empty() {
                exe.clone().unwrap_or_default()
            } else {
                parts.join(" ")
            }
        };

        let user = process.user_id().and_then(|uid| {
            users
                .get_or_insert_with(sysinfo::Users::new_with_refreshed_list)
                .get_user_by_id(uid)
                .map(|user| user.name().to_string())
        });

        // 版本信息读 exe：一次 stat 级别的开销，几百个进程可接受；
        // 无权限的系统进程直接跳过（返回 None，不阻塞）。
        let (description, company) = exe
            .as_deref()
            .and_then(|path| file_version_info(Path::new(path)))
            .unwrap_or((None, None));

        rows.push(ProcessRow {
            pid: pid.as_u32(),
            parent_pid: process.parent().map(|p| p.as_u32()),
            name: raw_name,
            exe,
            cmd,
            cpu: process.cpu_usage(),
            memory_mb: process.memory() as f64 / 1024.0 / 1024.0,
            user,
            status: format!("{:?}", process.status()),
            started_at: process.start_time(),
            description,
            company,
            critical,
        });
    }

    // 默认按 CPU 降序：用户打开这个面板就是为了找"谁在吃资源"。
    rows.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_mb.partial_cmp(&a.memory_mb).unwrap_or(std::cmp::Ordering::Equal))
    });

    let total = system.total_memory() as f64 / 1024.0 / 1024.0;
    let used = system.used_memory() as f64 / 1024.0 / 1024.0;
    let overview = SystemOverview {
        total_memory_mb: total,
        used_memory_mb: used,
        cpu_count: system.cpus().len(),
        process_count: rows.len(),
        cpu_usage: system.global_cpu_usage(),
        uptime_secs: sysinfo::System::uptime(),
        // 主机名/系统版本是静态信息，取失败就给空串（UI 不显示即可）。
        host_name: sysinfo::System::host_name().unwrap_or_default(),
        os_version: sysinfo::System::long_os_version().unwrap_or_default(),
    };

    ProcessSnapshot {
        processes: rows,
        overview,
    }
}

/// 轻量进程表：复用同一个 sampler（CPU 差分才有意义），但**不读 exe 版本信息**
/// ——那是几百次文件 IO，后台清理扫描用不上，只取打分需要的字段。
///
/// 复用 sampler 而不是新建 System 是必须的：新建会触发第二次 300ms
/// 差分 sleep，而且两个 System 的 CPU 基准不同，差分出来的百分比是错的。
#[derive(Debug, Clone)]
pub struct LightProcess {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub cpu: f32,
    pub memory_mb: f64,
    /// 累计读写字节（sysinfo 内部走 GetProcessIoCounters，零额外 Win32 开销）。
    pub disk_bytes: u64,
    pub started_at: u64,
    pub user: Option<String>,
}

pub fn refresh_light() -> Vec<LightProcess> {
    let mut guard = match sampler().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let since = guard.sampled_at.elapsed();
    if since < CPU_SAMPLE_GAP {
        std::thread::sleep(CPU_SAMPLE_GAP - since);
    }
    guard
        .system
        .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    guard.sampled_at = Instant::now();

    let system = &guard.system;
    let mut users: Option<sysinfo::Users> = None;
    let mut rows: Vec<LightProcess> = Vec::with_capacity(system.processes().len());
    for (pid, process) in system.processes() {
        let user = process.user_id().and_then(|uid| {
            users
                .get_or_insert_with(sysinfo::Users::new_with_refreshed_list)
                .get_user_by_id(uid)
                .map(|user| user.name().to_string())
        });
        let io = process.disk_usage();
        rows.push(LightProcess {
            pid: pid.as_u32(),
            parent_pid: process.parent().map(|p| p.as_u32()),
            name: process.name().to_string_lossy().to_string(),
            cpu: process.cpu_usage(),
            memory_mb: process.memory() as f64 / 1024.0 / 1024.0,
            disk_bytes: io.total_read_bytes + io.total_written_bytes,
            started_at: process.start_time(),
            user,
        });
    }
    rows
}

/// 结束进程。返回给用户的文案要能解释失败原因（权限/UAC/已退出）。
pub fn kill_process(pid: u32, force: bool) -> Result<String, String> {
    if pid <= 4 {
        return Err("PID 0/4 属于系统内核，禁止结束。".to_string());
    }

    let mut guard = match sampler().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .system
        .refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let Some(process) = guard.system.process(sysinfo::Pid::from_u32(pid)) else {
        return Err(format!("进程 {pid} 不存在（可能已退出）。"));
    };

    let name = process.name().to_string_lossy().to_string();
    if CRITICAL_NAMES
        .iter()
        .any(|n| name.to_ascii_lowercase() == *n)
    {
        return Err(format!(
            "{name} 是系统关键进程，结束它会导致蓝屏或注销，已拒绝。"
        ));
    }

    let ok = if force {
        process.kill()
    } else {
        process.kill_with(sysinfo::Signal::Term).unwrap_or_else(|| process.kill())
    };

    if ok {
        Ok(format!("已结束 {name}（PID {pid}）"))
    } else {
        Err(format!(
            "结束 {name}（PID {pid}）失败：权限不足。受保护进程需要以管理员身份运行本程序。"
        ))
    }
}

/// 磁盘列表：容量来自 `sysinfo::Disks`（一次 `GetLogicalDrives` + 卷信息）。
pub fn disks() -> Vec<DiskRow> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut rows: Vec<DiskRow> = disks
        .list()
        .iter()
        .map(|disk| {
            let mount = disk.mount_point().display().to_string();
            let drive = drive_of(&mount);
            DiskRow {
                name: disk.name().to_string_lossy().to_string(),
                mount,
                total_bytes: disk.total_space(),
                free_bytes: disk.available_space(),
                removable: disk.is_removable(),
                drive,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.drive.cmp(&b.drive));
    rows
}

/// `C:\` → `C:`（去掉尾部分隔符，作为前端过滤键与清理规则的盘符单位）。
pub fn drive_of(mount: &str) -> String {
    let trimmed = mount.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        trimmed.to_ascii_uppercase()
    } else {
        trimmed.to_string()
    }
}

/// 读取 exe 的版本信息：`(FileDescription, CompanyName)`。
#[cfg(windows)]
fn file_version_info(path: &Path) -> Option<(Option<String>, Option<String>)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    // 只对可执行文件读版本信息：给 .dll/.exe 之外的路径调用是纯浪费。
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "exe" | "dll" | "sys" | "com") {
        return None;
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut handle: u32 = 0;
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    let ok = unsafe {
        GetFileVersionInfoW(
            wide.as_ptr(),
            handle,
            size,
            buffer.as_mut_ptr() as *mut core::ffi::c_void,
        )
    };
    if ok == 0 {
        return None;
    }

    let query = |key: &str| -> Option<String> {
        let sub: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = unsafe {
            VerQueryValueW(
                buffer.as_ptr() as *const core::ffi::c_void,
                sub.as_ptr(),
                &mut ptr,
                &mut len,
            )
        };
        if ok == 0 || ptr.is_null() || len == 0 {
            return None;
        }
        // 版本信息里的字符串是 UTF-16，len 是**字符数**（含结尾 NUL）。
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize) };
        let text = String::from_utf16_lossy(slice)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    };

    let description = query(r"\StringFileInfo\040904B0\FileDescription")
        .or_else(|| query(r"\StringFileInfo\080404B0\FileDescription"));
    let company = query(r"\StringFileInfo\040904B0\CompanyName")
        .or_else(|| query(r"\StringFileInfo\080404B0\CompanyName"));

    Some((description, company))
}

#[cfg(not(windows))]
fn file_version_info(_path: &Path) -> Option<(Option<String>, Option<String>)> {
    None
}

/// 供清理模块复用：把 `%VAR%` 展开成真实路径（大小写不敏感）。
pub fn expand_env(template: &str) -> Option<PathBuf> {
    let mut out = String::new();
    let bytes: Vec<char> = template.chars().collect();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == '%' {
            let rest = &bytes[index + 1..];
            if let Some(end) = rest.iter().position(|c| *c == '%') {
                let name: String = rest[..end].iter().collect();
                let value = std::env::var(&name).ok()?;
                out.push_str(&value);
                index += end + 2;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    Some(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_key_normalizes_mount_points() {
        assert_eq!(drive_of("C:\\"), "C:");
        assert_eq!(drive_of("d:/"), "D:");
        assert_eq!(drive_of("\\\\server\\share"), "\\\\server\\share");
    }

    #[test]
    fn expands_environment_variables_case_insensitively() {
        std::env::set_var("HERMIX_SYSMON_TEST", r"C:\tmp");
        // Windows 环境变量名不区分大小写，这里用小写拼写验证展开走的是系统语义。
        let path = expand_env("%hermix_sysmon_test%\\x").expect("expanded");
        assert!(path.to_string_lossy().ends_with(r"x"));
    }

    #[test]
    fn critical_processes_are_never_killed() {
        assert!(kill_process(4, true).is_err());
    }

    #[test]
    fn snapshot_exposes_current_process() {
        let snap = snapshot();
        assert!(!snap.processes.is_empty(), "至少能看到自己");
        let me = std::process::id();
        assert!(
            snap.processes.iter().any(|row| row.pid == me),
            "快照里应当包含当前进程"
        );
        assert!(snap.overview.total_memory_mb > 0.0);
    }
}
