//! 后台进程自动清理 —— 启发式打分 + 安全网 + 两级退出。
//!
//! 算法出处（开源调研，详见决策笔记
//! `docs/notes/implemented/feature/2026-10-23-system-tuner.md`）：
//! - **Idlekiller**（Poseidoncode/Idlekiller，Rust+sysinfo，MIT）：最贴近的开源
//!   先例——「空闲 + 占内存 + 零 I/O」三条件启发式 + 五层保护网 + 批量清理的
//!   爆炸半径保护。它用的正是本仓库同款 `sysinfo`，判定逻辑可直接移植。
//! - **Monit**：资源阈值 + 宽限期（先请求停止，等一段再强杀）。
//! - **gnome-system-monitor**：先优雅（TERM）后强杀（KILL），强杀前必须确认。
//! - **Process Lasso Watchdog**：阈值 + 持续时间 → 动作。
//! - **Autoruns**：安全网要「问系统」（签名/关键进程），而不是维护名字表。
//!
//! 本模块与磁盘清理模块（`cleanup.rs`）是同一套范式：
//! **扫描 → 打分 → 列出候选与理由 → 用户确认 → 执行**，前端路径一律不信，
//! 执行时服务端重新校验。
//!
//! 四个必须记住的 Windows 语义（调研实测，决定了本模块怎么写）：
//! 1. `GetLastInputInfo` 是**会话级全局空闲**，不是 per-process —— 只能当
//!    全局闸门（"系统已 24 小时无输入"），不能说"这个进程 24 小时没动"。
//! 2. `sysinfo` 在 Windows 上 `Process::status()` 恒为 `Run`，所以 Idlekiller
//!    的 Sleep/Idle 判定在 Windows **不可用** → 改用「无可见窗口 + 零 I/O +
//!    低 CPU + 全局空闲」的组合。
//! 3. `sysinfo` 内部已用 `GetProcessIoCounters` → per-process 读写字节免费
//!    拿到（`Process::disk_usage()`），不用自己调 Win32。
//! 4. `IsProcessCritical`（kernel32，Win8.1+）能直接问系统"这是不是关键
//!    进程" —— 本模块用它替掉「只靠名字名单」的旧做法。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

use crate::sysmon;

/// 建议清理的分数门槛（满分 100）。
const SCORE_RECOMMEND: i32 = 70;
/// 进入候选清单的分数门槛（低于此视为"正常进程"，不打扰用户）。
const SCORE_LISTED: i32 = 40;

/// 保护名单：用户正在用的前台类程序，**永不**进清理候选。
/// 匹配方式用 Idlekiller 的**分词全等**（`xterminal` 不算 `terminal`），
/// 避免子串匹配造成的误伤。
const PROTECTED_NAMES: &[&str] = &[
    // 终端 / shell / 命令行
    "cmd", "powershell", "pwsh", "conhost", "windowsterminal", "terminal", "bash", "wsl", "git",
    // 桌面与外壳
    "explorer", "dwm", "shellexperiencehost", "startmenuexperiencehost", "searchhost",
    "searchapp", "taskmgr", "taskhostw",
    // 编辑器 / IDE
    "code", "devenv", "idea", "idea64", "pycharm", "pycharm64", "webstorm", "notepad", "notepad++",
    "vim", "nvim", "emacs", "sublime_text",
    // 浏览器
    "chrome", "msedge", "firefox", "brave", "opera", "vivaldi",
    // 即时通讯
    "wechat", "weixin", "wxwork", "qq", "dingtalk", "feishu", "lark", "slack", "discord",
    "telegram",
    // 影音 / 游戏
    "steam", "steamwebhelper", "potplayer", "vlc", "obs64", "obs32",
    // 本程序自身（结束自己等于自杀）
    "hermix", "hermix-desktop", "hermix-viewer", "hermix-filesearch", "hermix-server",
];

/// 风险档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    /// 建议清理：启发式达标且安全网全过。
    Low,
    /// 需要人工判断：有一定嫌疑但证据不足。
    Medium,
    /// 不建议动：命中安全网或分数不够。
    High,
}

/// 一个候选进程（进清理清单的才返回）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub pid: u32,
    pub name: String,
    /// 内存占用（MB）。
    pub memory_mb: f64,
    /// 分数（0-100）。
    pub score: i32,
    pub risk: RiskLevel,
    /// 给用户看的一句话理由串（为什么它被选中）。
    pub reasons: Vec<String>,
    /// 是否落在"建议清理"档（前端据此默认勾选）。
    pub recommended: bool,
    /// 被安全网拦下的原因（拦下了也给用户看，别让用户以为"扫不出来"）。
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub candidates: Vec<Candidate>,
    /// 落在建议档的条数。
    pub recommended_count: usize,
    /// 建议档合计可释放的内存（MB）。
    pub reclaim_mb: f64,
    /// 系统（会话）已空闲多少秒 —— 全局值，不是某个进程的空闲时长。
    pub idle_secs: u64,
    /// 本次扫描到的进程总数。
    pub scanned: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutcome {
    pub pid: u32,
    pub name: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecReport {
    pub outcomes: Vec<ExecOutcome>,
    pub ended: usize,
    pub failed: usize,
    pub freed_mb: f64,
}

// ── 上一次的 I/O 计数（用于判定"这一轮没有任何磁盘活动"）─────────

fn last_io_map() -> &'static Mutex<HashMap<u32, u64>> {
    static MAP: OnceLock<Mutex<HashMap<u32, u64>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Tauri 命令 ────────────────────────────────────────────────────────

/// 扫描后台可清理进程（只读，不动任何进程）。
#[tauri::command]
pub async fn sysmon_cleanup_scan() -> Result<ScanReport, String> {
    tauri::async_runtime::spawn_blocking(scan)
        .await
        .map_err(|error| format!("后台进程扫描失败：{error}"))
}

/// 结束选中的候选进程。服务端重新跑一遍安全网，前端传什么都不能绕过。
#[tauri::command]
pub async fn sysmon_cleanup_execute(pids: Vec<u32>) -> Result<ExecReport, String> {
    tauri::async_runtime::spawn_blocking(move || execute(&pids))
        .await
        .map_err(|error| format!("结束进程失败：{error}"))
}

// ── 扫描与打分 ───────────────────────────────────────────────────────

pub fn scan() -> ScanReport {
    let started = Instant::now();
    let processes = sysmon::refresh_light();

    // 三次全局枚举（各一次，绝不 per-process 调重 API）：
    // 有可见窗口的 PID / 服务宿主 PID / 会话空闲秒数。
    let (window_pids, _) = visible_windows();
    let service_pids = service_host_pids();
    let idle_secs = session_idle_secs();

    let self_pid = std::process::id();
    let parent_of = |pid: u32| -> Option<u32> {
        processes.iter().find(|p| p.pid == pid).and_then(|p| p.parent_pid)
    };
    // 自身祖先链：结束祖先 = 结束自己。
    let mut ancestors: HashSet<u32> = HashSet::new();
    let mut cursor = parent_of(self_pid);
    while let Some(pid) = cursor {
        if !ancestors.insert(pid) {
            break;
        }
        cursor = parent_of(pid);
    }

    let mut previous_io = last_io_map().lock().unwrap();
    let mut current_io: HashMap<u32, u64> = HashMap::with_capacity(processes.len());
    let mut candidates: Vec<Candidate> = Vec::new();

    for process in &processes {
        current_io.insert(process.pid, process.disk_bytes);
        // 这一轮相对上一轮**没有新增磁盘活动**（首次见到该进程不算）。
        let io_idle = match previous_io.get(&process.pid) {
            Some(prev) => *prev == process.disk_bytes,
            None => false,
        };
        if process.pid == self_pid || ancestors.contains(&process.pid) {
            continue;
        }
        let lower = process.name.to_ascii_lowercase();
        let mut reasons: Vec<String> = Vec::new();
        let mut score: i32 = 0;

        // ── 软打分（先算分，再决定要不要花贵 API 去查安全网）────────
        if process.memory_mb >= 500.0 {
            score += 40;
            reasons.push(format!("占用内存 {} MB", process.memory_mb.round() as i64));
        } else if process.memory_mb >= 200.0 {
            score += 25;
            reasons.push(format!("占用内存 {} MB", process.memory_mb.round() as i64));
        } else if process.memory_mb >= 100.0 {
            score += 10;
            reasons.push(format!("占用内存 {} MB", process.memory_mb.round() as i64));
        }
        if io_idle {
            score += 20;
            reasons.push("无磁盘读写活动".to_string());
        }
        if process.cpu < 0.5 {
            score += 15;
            reasons.push("几乎不占 CPU".to_string());
        }
        if !window_pids.contains(&process.pid) {
            score += 15;
            reasons.push("没有可见窗口".to_string());
        }
        if idle_secs >= 86_400 {
            score += 20;
            reasons.push("系统已 24 小时无操作".to_string());
        } else if idle_secs >= 1_800 {
            score += 10;
            reasons.push("系统已 30 分钟无操作".to_string());
        }
        let uptime = sysinfo::System::uptime().saturating_sub(process.started_at);
        if uptime >= 3_600 {
            score += 5;
            reasons.push("已运行 1 小时以上".to_string());
        }

        if score < SCORE_LISTED {
            continue;
        }

        // ── 安全网（硬门槛）：任一命中就否决，且把原因告诉用户 ──────
        let blocked = safety_block(
            process.pid,
            &lower,
            &window_pids,
            &service_pids,
            &ancestors,
        );

        let recommended = blocked.is_none() && score >= SCORE_RECOMMEND;
        let risk = if blocked.is_some() || score < SCORE_LISTED {
            RiskLevel::High
        } else if recommended {
            RiskLevel::Low
        } else {
            RiskLevel::Medium
        };
        candidates.push(Candidate {
            pid: process.pid,
            name: process.name.clone(),
            memory_mb: (process.memory_mb * 10.0).round() / 10.0,
            score,
            risk,
            reasons,
            recommended,
            blocked,
        });
    }
    *previous_io = current_io;
    drop(previous_io);

    // 建议档在前，其次按分数、再按内存。
    candidates.sort_by(|a, b| {
        b.recommended
            .cmp(&a.recommended)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| {
                b.memory_mb
                    .partial_cmp(&a.memory_mb)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let recommended_count = candidates.iter().filter(|c| c.recommended).count();
    let reclaim_mb: f64 = candidates
        .iter()
        .filter(|c| c.recommended)
        .map(|c| c.memory_mb)
        .sum();

    ScanReport {
        candidates,
        recommended_count,
        reclaim_mb: (reclaim_mb * 10.0).round() / 10.0,
        idle_secs,
        scanned: processes.len(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// 安全网：返回 `Some(原因)` 表示这个进程**不能**被自动清理。
///
/// 权威判定靠「问系统」（关键进程 / 提权 / 服务宿主 / 有窗口），
/// 名字名单只做兜底 —— `IsProcessCritical` 对部分进程会返回 FALSE，
/// 且无权限时 `OpenProcess` 直接失败，两层同时失效时名字是最后一道防线。
fn safety_block(
    pid: u32,
    lower_name: &str,
    window_pids: &HashSet<u32>,
    service_pids: &HashSet<u32>,
    ancestors: &HashSet<u32>,
) -> Option<String> {
    if pid <= 4 {
        return Some("系统内核进程".to_string());
    }
    if ancestors.contains(&pid) {
        return Some("本程序的父级进程".to_string());
    }
    if is_critical_process(pid) {
        return Some("系统关键进程".to_string());
    }
    if is_elevated(pid) {
        return Some("以管理员权限运行，结束可能影响系统".to_string());
    }
    if service_pids.contains(&pid) {
        return Some("系统服务的宿主进程（应重启服务而非结束）".to_string());
    }
    if window_pids.contains(&pid) {
        return Some("当前有可见窗口，用户可能正在使用".to_string());
    }
    if is_protected_name(lower_name) {
        return Some("属于用户常用程序，不自动结束".to_string());
    }
    None
}

/// 名单匹配：分词全等（Idlekiller 的做法），避免 `xterminal` 误伤 `terminal`。
fn is_protected_name(lower_name: &str) -> bool {
    let stem = lower_name.strip_suffix(".exe").unwrap_or(lower_name);
    if PROTECTED_NAMES.iter().any(|p| *p == stem) {
        return true;
    }
    stem.split(|c: char| !c.is_alphanumeric())
        .any(|part| PROTECTED_NAMES.iter().any(|p| *p == part))
}

// ── 执行 ─────────────────────────────────────────────────────────────

pub fn execute(pids: &[u32]) -> ExecReport {
    let processes = sysmon::refresh_light();
    let (window_pids, window_handles) = visible_windows();
    let service_pids = service_host_pids();
    let self_pid = std::process::id();

    // 自身祖先链：结束祖先 = 结束自己。执行时也要算一遍（扫描时的链可能已变）。
    let parent_of = |pid: u32| -> Option<u32> {
        processes.iter().find(|p| p.pid == pid).and_then(|p| p.parent_pid)
    };
    let mut ancestors: HashSet<u32> = HashSet::new();
    let mut cursor = parent_of(self_pid);
    while let Some(pid) = cursor {
        if !ancestors.insert(pid) {
            break;
        }
        cursor = parent_of(pid);
    }

    let mut outcomes: Vec<ExecOutcome> = Vec::new();
    let mut ended = 0usize;
    let mut failed = 0usize;
    let mut freed_mb = 0.0f64;

    for pid in pids {
        let Some(process) = processes.iter().find(|p| p.pid == *pid) else {
            failed += 1;
            outcomes.push(ExecOutcome {
                pid: *pid,
                name: String::new(),
                ok: false,
                message: "进程已退出".to_string(),
            });
            continue;
        };
        let lower = process.name.to_ascii_lowercase();
        let blocked = safety_block(
            process.pid,
            &lower,
            &window_pids,
            &service_pids,
            &ancestors,
        )
        .or(if process.pid == self_pid {
            Some("不能结束自身".to_string())
        } else {
            None
        });
        if let Some(reason) = blocked {
            failed += 1;
            outcomes.push(ExecOutcome {
                pid: process.pid,
                name: process.name.clone(),
                ok: false,
                message: format!("已拒绝：{reason}"),
            });
            continue;
        }

        // 优雅退出优先：给窗口发 WM_CLOSE，等宽限期后仍存活再强杀
        // （Monit / gnome-system-monitor 的同一阶梯）。
        let grace = graceful_stop(&window_handles, process.pid);
        if grace == Grace::Exited {
            ended += 1;
            freed_mb += process.memory_mb;
            outcomes.push(ExecOutcome {
                pid: process.pid,
                name: process.name.clone(),
                ok: true,
                message: "已请求退出，进程自行结束".to_string(),
            });
            continue;
        }
        match force_terminate(process.pid) {
            Ok(()) => {
                ended += 1;
                freed_mb += process.memory_mb;
                outcomes.push(ExecOutcome {
                    pid: process.pid,
                    name: process.name.clone(),
                    ok: true,
                    message: "已结束".to_string(),
                });
            }
            Err(message) => {
                failed += 1;
                outcomes.push(ExecOutcome {
                    pid: process.pid,
                    name: process.name.clone(),
                    ok: false,
                    message,
                });
            }
        }
    }

    ExecReport {
        outcomes,
        ended,
        failed,
        freed_mb: (freed_mb * 10.0).round() / 10.0,
    }
}

#[derive(PartialEq)]
enum Grace {
    Exited,
    StillAlive,
}

/// 给该进程的窗口发 WM_CLOSE，等宽限期后看它还在不在。
fn graceful_stop(windows: &HashMap<u32, Vec<isize>>, pid: u32) -> Grace {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

        let Some(handles) = windows.get(&pid) else {
            return Grace::StillAlive;
        };
        for hwnd in handles {
            unsafe {
                PostMessageW(*hwnd, WM_CLOSE, 0, 0);
            }
        }
        // 宽限期 1.5s：后台进程通常没有消息循环，这一档基本只对窗口程序有效。
        let deadline = Instant::now() + std::time::Duration::from_millis(1_500);
        while Instant::now() < deadline {
            if !process_alive(pid) {
                return Grace::Exited;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        if !process_alive(pid) {
            return Grace::Exited;
        }
    }
    Grace::StillAlive
}

fn process_alive(pid: u32) -> bool {
    sysinfo::System::new_all()
        .process(sysinfo::Pid::from_u32(pid))
        .is_some()
}

fn force_terminate(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle == 0 {
                return Err("结束失败：权限不足（受保护进程需要管理员权限）".to_string());
            }
            let ok = TerminateProcess(handle, 0);
            CloseHandle(handle);
            if ok == 0 {
                return Err("结束失败：系统拒绝了终止请求".to_string());
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err("当前平台不支持结束进程".to_string())
    }
}

// ── Windows 采集 ─────────────────────────────────────────────────────

/// 全局枚举一次顶层窗口 → (有可见窗口的 PID 集合, PID → 窗口句柄)。
fn visible_windows() -> (HashSet<u32>, HashMap<u32, Vec<isize>>) {
    #[cfg(windows)]
    {
        use std::collections::HashMap;
        use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, GetWindowTextW, IsWindowVisible,
        };

        let mut map: HashMap<u32, Vec<isize>> = HashMap::new();
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let map = unsafe { &mut *(lparam as *mut HashMap<u32, Vec<isize>>) };
            if IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            // 空标题的窗口多半是工具窗口/托盘气泡，不当作"用户在用"。
            let mut title = [0u16; 256];
            let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            if len <= 0 {
                return 1;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != 0 {
                map.entry(pid).or_default().push(hwnd);
            }
            1
        }
        unsafe {
            let _ = EnumWindows(Some(collect), &mut map as *mut _ as isize);
        }
        let pids: HashSet<u32> = map.keys().copied().collect();
        return (pids, map);
    }
    #[cfg(not(windows))]
    {
        (HashSet::new(), HashMap::new())
    }
}

/// 服务宿主进程 PID 集合（这些进程要"重启服务"而不是"结束"）。
fn service_host_pids() -> HashSet<u32> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FALSE;
        use windows_sys::Win32::Security::SC_HANDLE;
        use windows_sys::Win32::System::Services::{
            EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW,
            SC_ENUM_PROCESS_INFO, SC_MANAGER_ALL_ACCESS, SERVICE_STATE_ALL, SERVICE_WIN32,
        };

        let mut pids = HashSet::new();
        unsafe {
            let manager: SC_HANDLE = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS);
            if manager == 0 {
                return pids;
            }
            // 先问需要多少字节，再取数据（两段式是 EnumServicesStatusEx 的规矩）。
            let mut needed: u32 = 0;
            let mut returned: u32 = 0;
            let first = EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                std::ptr::null_mut(),
                0,
                &mut needed,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if first == FALSE || needed == 0 {
                let _ = windows_sys::Win32::System::Services::CloseServiceHandle(manager);
                return pids;
            }
            let mut buffer = vec![0u8; needed as usize];
            let ok = EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr(),
                needed,
                &mut needed,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            let _ = windows_sys::Win32::System::Services::CloseServiceHandle(manager);
            if ok == FALSE {
                return pids;
            }
            let count = returned as usize;
            let stride = std::mem::size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
            for index in 0..count {
                let offset = index * stride;
                if offset + stride > buffer.len() {
                    break;
                }
                let entry = &*(buffer.as_ptr().add(offset) as *const ENUM_SERVICE_STATUS_PROCESSW);
                let pid = entry.ServiceStatusProcess.dwProcessId;
                if pid != 0 {
                    pids.insert(pid);
                }
            }
        }
        pids
    }
    #[cfg(not(windows))]
    {
        HashSet::new()
    }
}

/// 会话级空闲秒数（**全局**值，不是某个进程的空闲时长）。
fn session_idle_secs() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetTickCount64;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        unsafe {
            let mut info = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            if GetLastInputInfo(&mut info) == 0 {
                return 0;
            }
            let now = GetTickCount64();
            // dwTime 是 u32 毫秒计数，会回绕；用 u64 的 now 无法直接相减，
            // 这里按 32 位回绕语义算差值。
            let idle_ms = (now as u32).wrapping_sub(info.dwTime) as u64;
            idle_ms / 1_000
        }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 系统是否把该进程标记为「关键进程」（内核权威判定，Win8.1+）。
fn is_critical_process(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
        use windows_sys::Win32::System::Threading::{
            IsProcessCritical, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if handle == 0 {
                // 拿不到句柄（无权限）时不能当成"关键"，但也不能当成"安全"，
                // 交给后面的名字兜底与提权判定。
                return false;
            }
            let mut critical = FALSE;
            let ok = IsProcessCritical(handle, &mut critical);
            CloseHandle(handle);
            ok != FALSE && critical != FALSE
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

/// 进程是否以管理员（提权）令牌运行。
fn is_elevated(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if process == 0 {
                return false;
            }
            let mut token: windows_sys::Win32::Foundation::HANDLE = 0;
            let ok = OpenProcessToken(process, TOKEN_QUERY, &mut token);
            CloseHandle(process);
            if ok == FALSE || token == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size: u32 = 0;
            let got = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            );
            CloseHandle(token);
            got != FALSE && elevation.TokenIsElevated != 0
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_name_matches_full_token_only() {
        // 分词全等：xterminal 不算 terminal（Idlekiller 的做法）
        assert!(is_protected_name("terminal"));
        assert!(is_protected_name("windows terminal.exe"));
        assert!(!is_protected_name("xterminal"));
        assert!(is_protected_name("code.exe"));
        assert!(!is_protected_name("codex"));
    }

    #[test]
    fn score_thresholds_are_ordered() {
        assert!(SCORE_RECOMMEND > SCORE_LISTED);
        assert!(SCORE_LISTED > 0);
    }

    #[test]
    fn candidate_serializes_camel_case() {
        let c = Candidate {
            pid: 1,
            name: "x".into(),
            memory_mb: 1.0,
            score: 10,
            risk: RiskLevel::Low,
            reasons: vec![],
            recommended: true,
            blocked: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"memoryMb\""));
        assert!(json.contains("\"risk\":\"low\""));
    }
}
