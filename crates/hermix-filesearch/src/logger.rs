//! 进程日志：GUI 子系统进程没有 stderr。
//!
//! **为什么需要它**：`hermix-filesearch.exe` 以 `windows_subsystem = "windows"`
//! 编译（见 `bin/daemon.rs`）——只有这样才能保证每次被提权拉起时**绝不弹
//! 黑色命令行窗口**。代价是这个进程没有控制台：`eprintln!` 写 stderr 会
//! 失败，而 Rust 的 `eprintln!` 对写失败是 **panic**（不是忽略），一个诊断
//! 语句就能把索引服务打死。
//!
//! 所以：所有诊断信息落到数据目录下的 `filesearch.log`，格式带毫秒时间戳，
//! 一次启动一段分隔线，文件超过上限就在下次启动时清空（诊断日志只需要
//! "最近一次运行"，不是审计档案）。

use std::{
    fmt::Arguments,
    fs::OpenOptions,
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// 日志上限：超过就在下次启动时清空。
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<std::fs::File>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// 打开日志文件。`data_dir` 就是索引数据目录，日志与索引同处一地。
///
/// 必须在任何 [`log!`] 之前调用；未初始化时 [`log!`] 静默丢弃（而不是
/// panic）——日志永远不该成为让服务起不来的原因。
pub fn init(data_dir: &Path) {
    let path = data_dir.join("filesearch.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "---- 启动 {} ----", stamp());
    let _ = file.flush();
    if let Ok(mut slot) = sink().lock() {
        *slot = Some(file);
    }
}

/// 写一行日志（自动补时间戳前缀）。
pub fn write_line(text: &str) {
    let Ok(mut slot) = sink().lock() else {
        return;
    };
    let Some(file) = slot.as_mut() else {
        return;
    };
    let _ = writeln!(file, "[{}] {}", stamp(), text);
    let _ = file.flush();
}

/// 写一条格式化日志。
pub fn write_fmt(args: Arguments<'_>) {
    write_line(&format!("{args}"));
}

/// 日志用的时间戳：`HH:MM:SS.mmm`（本机时区）。
///
/// 只做"当天时刻"而不是完整日期：跨天的长驻进程靠启动分隔线区分。
fn stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let seconds = now / 1000;
    let millis = now % 1000;
    let day_seconds = seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
        millis
    )
}

/// 记一条日志。
///
/// ```ignore
/// crate::log!("索引完成：{} 项", count);
/// ```
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::logger::write_fmt(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_has_hhmmss_mmm_shape() {
        let text = stamp();
        let parts: Vec<&str> = text.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].len() == 2 && parts[1].len() == 2);
        assert!(parts[2].contains('.'));
    }

    /// 未初始化时写日志不能炸——日志失败不该让服务起不来。
    #[test]
    fn writing_without_init_is_a_noop() {
        write_line("这条不该 panic");
    }
}
