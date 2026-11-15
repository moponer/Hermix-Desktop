//! 桌面端（Tauri shell）自己的诊断日志落地。
//!
//! 为什么存在：sidecar 的诊断过去全靠 `eprintln!`，而桌面端是 GUI 进程、
//! 没有 stderr 去处 —— "[Hermix] Sidecar health check failed"、
//! "cleanup: stopping sidecar (trigger=…)" 这些唯一的重启证据落地即丢。
//! 2026-11-05 排查「sidecar 一天被看门狗重启 19 次」时被迫在零证据下推断，
//! 本模块就是那次教训的直接产物。
//!
//! 写到 `~/.hermix/logs/desktop.log`（与 server.log 同目录，时间线可直接
//! 对齐）。追加写 + 简单大小滚动（>5MB 时整体截断重开，诊断日志不值得
//! 更精细的轮转）。每条调用同时保留 `eprintln!`，不改变现有行为。
//!
//! 无 chrono：viewer 侧已有先例（commands.rs::chrono_like），这里用同款
//! 极简 unix 秒 → 本地时间换算。注意它算的是**本地时区**由运行时给出
//! 偏移不可得，退而求其次打 UTC+8（本产品的唯一部署时区，见 AGENTS 约定
//! 「主人在中国区」）并显式标注 +08:00，避免误导对时。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// 追加锁：多线程看门狗 / 命令处理器并发写同一文件。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 日志文件路径：`<hermix_home>/logs/desktop.log`。
///
/// 必须与 sidecar（`hermix_home()`）和前端上报（`frontend_log.rs`）落在
/// **同一个**目录：此前这里硬编码 `~/.hermix`，而 `frontend.log` 走
/// `resolve_data_dir()` 落到安装目录，一次运行的两半日志对不上时间线。
/// 走 `hermix_home()` 后三者共用一条解析链（portable env → `HERMIX_HOME`
/// → `~/.hermix`），便携模式与开发模式也自动一致。
fn log_path() -> Option<PathBuf> {
    let home = hermix_config::paths::hermix_home();
    Some(home.join("logs").join("desktop.log"))
}

/// unix 秒 → 本地(UTC+8)时间字符串 `MM-DD HH:MM:SS`。
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = secs + 8 * 3600;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days（Howard Hinnant 算法），viewer/commands.rs 同款。
    // 闰修正项与日期推导统一收在 civil_from_days（含回归测试）。
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} +08:00")
}

/// `civil_from_days` 的纯函数化：unix 天数 → (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    if mo <= 2 {
        y += 1;
    }
    (y, mo as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    /// 2026-11-05 的实机回归：闰修正项写成 yoe/1460 时这里返回
    /// (2026, 11, 11)（+6 天），desktop.log 全部时间戳错位。
    #[test]
    fn civil_from_days_matches_known_dates() {
        // 2026-11-05 = unix 天数 20762
        assert_eq!(civil_from_days(20762), (2026, 11, 5));
        // 闰日：2024-02-29 = unix 天数 19782
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        // 纪元起点
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }
}

/// 落地一条桌面端诊断日志：stderr + `~/.hermix/logs/desktop.log` 各一份。
pub fn log(msg: &str) {
    eprintln!("{msg}");
    append_file(msg);
}

fn append_file(msg: &str) {
    let Some(path) = log_path() else { return };
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 简单大小滚动：超过 5MB 重开（截断）。诊断日志不需要更精细的轮转，
        // 但不能无限长 —— 看门狗每 5s 一条失败日志，一年就是 30MB+。
        if path.metadata().map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false) {
            std::fs::write(&path, b"")?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{} {}", timestamp(), msg)
    })();
    if let Err(e) = result {
        // 写不进去也别再打扰 stderr 主线（一次即可，失败通常是磁盘问题）。
        eprintln!("[Hermix:desktop_log] append failed: {e}");
    }
}

/// `eprintln!` 的直替品：参数形态完全一致，多落一份文件。
#[macro_export]
macro_rules! desktop_log {
    ($($arg:tt)*) => {
        $crate::desktop_log::log(&format!($($arg)*))
    };
}

/// 启动时调用（lib.rs 在一切副作用之前调它）：确保日志目录存在、
/// 写入一条进程启动分隔线。之后所有 `desktop_log!` 走 append 通道。
pub fn init() {
    append_file("── desktop process started ──");
}
