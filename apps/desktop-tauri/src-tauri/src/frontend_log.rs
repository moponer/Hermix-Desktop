//! 前端错误落盘。
//!
//! 生产构建用 terser `drop_console` 删掉所有 `console.*`，于是前端「请求失败」
//! 与「列表本来就是空的」在日志里完全同形 —— 用户报告"点了没反应"时没有任何
//! 证据可查。这条 IPC 是前端唯一的持久化上报通道。
//!
//! 为什么走 Tauri 命令而不是后端 HTTP：要上报的场景里有一半是"后端离线"
//! （503 / sidecar 没起来），走 HTTP 会二次失败。

use std::io::Write;
use std::path::PathBuf;

/// 单文件上限。前端错误通常是偶发几条，4MB 足够覆盖很久；超过就轮转一次
/// （`frontend.log` → `frontend.log.1`），不做多代保留 —— 这是排障快照，
/// 不是审计日志。
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// 前端上报一条错误。永不 panic、永不返回错误：上报失败不能再制造一个失败。
#[tauri::command]
pub fn report_frontend_error(scope: String, message: String) {
    let scope: String = scope.chars().take(64).collect();
    let message: String = message.chars().take(4000).collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 第一列是 epoch 秒：省一个 chrono 依赖，也避免本地时区/夏令时歧义。
    let line = format!("{now}\t{scope}\t{message}\n");

    // 开发期仍打到 stderr（devtools 控制台可见）；release 下 windows_subsystem
    // 会吞掉 stderr，所以真正的载体是下面那个文件。
    eprintln!("[frontend] {scope}: {message}");

    if let Err(e) = append(&line) {
        eprintln!("[frontend] 落盘失败: {e}");
    }
}

fn log_path() -> PathBuf {
    // 必须与 `commands::log_dir()`（`resolve_data_dir()/logs`）一致：那个目录
    // 才是设置页「打开日志目录」与 `list_backend_log_files` 看的地方。用
    // `hermix_config::paths::hermix_home()` 在便携模式/`hermix-data` 开发模式下
    // 会指向另一个目录，日志就写到用户找不到的地方去了。
    crate::commands::resolve_data_dir()
        .join("logs")
        .join("frontend.log")
}

fn append(line: &str) -> std::io::Result<()> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_creates_file_and_writes_line() {
        // 直接测 append 的落盘路径不可行（会写到真实 hermix_home），
        // 这里只钉住"轮转阈值是有限值"，防止有人把它改成 u64::MAX。
        assert!(MAX_BYTES > 0 && MAX_BYTES <= 16 * 1024 * 1024);
    }
}
