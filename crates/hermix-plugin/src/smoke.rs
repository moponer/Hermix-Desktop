//! 运行时冒烟测试 (L2) — 启动打包后的壳加载应用, 捕获运行时错误.
//!
//! 壳在 `HERMIX_SMOKE=1` 时进入冒烟模式: 隐藏窗口, 并通过 `smoke_report`
//! IPC 把 JS 运行时错误 / 挂载结果写入 `HERMIX_SMOKE_LOG` 指定的日志文件.
//! 本模块负责启动该壳、轮询日志、解析结果, 供 agent 工具 (`custom_app_manager`
//! 的 `smoke` action) 与桌面 Tauri 命令 (`custom_app_smoke_test`) 共用.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 单条运行时错误.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeError {
    pub message: String,
}

/// 运行时冒烟测试结果.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SmokeReport {
    /// 是否通过 (已挂载且无运行时错误)
    pub ok: bool,
    /// 应用是否成功挂载 (React 渲染出内容)
    pub ready: bool,
    /// 是否已打包 (dist 存在)
    pub packaged: bool,
    /// 运行时错误列表
    pub errors: Vec<SmokeError>,
    /// 详细日志 (每行一条事件)
    pub detail: String,
}

/// 壳 exe 在不同平台下的文件名.
fn dist_exe_name(id: &str) -> String {
    if cfg!(windows) {
        format!("{id}.exe")
    } else {
        id.to_string()
    }
}

/// 解析冒烟日志内容 → (ready, errors, detail_lines).
///
/// 日志每行是一条 JSON: `{"level":"ready"|"error"|..., "message":"..."}`.
pub fn parse_smoke_log(content: &str) -> (bool, Vec<SmokeError>, Vec<String>) {
    let mut ready = false;
    let mut errors: Vec<SmokeError> = Vec::new();
    let mut detail: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("");
        let message = v
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        match level {
            "ready" => {
                ready = true;
                detail.push(format!("[ready] {message}"));
            }
            "error" => {
                detail.push(format!("[error] {message}"));
                errors.push(SmokeError { message });
            }
            other => detail.push(format!("[{other}] {message}")),
        }
    }
    (ready, errors, detail)
}

/// 启动壳并冒烟测试指定应用. 同步阻塞, 最多约 20s.
///
/// 调用方应先确保已打包 (`manager.package_plugin`). `app_dir` 为插件目录
/// (`<plugins_dir>/<id>`), 其下 `dist/` 必须存在并有 `<id>.exe`.
pub fn run_smoke_test(app_dir: &Path, _shell_path: &Path) -> SmokeReport {
    let id = app_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let dist_dir = app_dir.join("dist");
    let exe_path = dist_dir.join(dist_exe_name(&id));

    if !exe_path.exists() {
        return SmokeReport {
            ok: false,
            ready: false,
            packaged: false,
            errors: vec![SmokeError {
                message: format!("未找到打包产物: {} (请先打包应用)", exe_path.display()),
            }],
            detail: String::new(),
        };
    }

    let log_path = std::env::temp_dir().join(format!(
        "hermix_smoke_{}_{}.log",
        id,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&log_path);
    let log_str = log_path.to_string_lossy().to_string();

    let mut child = match Command::new(&exe_path)
        .current_dir(&dist_dir)
        .env("HERMIX_SMOKE", "1")
        .env("HERMIX_SMOKE_LOG", &log_str)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return SmokeReport {
                ok: false,
                ready: false,
                packaged: true,
                errors: vec![SmokeError {
                    message: format!("启动壳失败: {e}"),
                }],
                detail: String::new(),
            };
        }
    };

    let start = Instant::now();
    let timeout = Duration::from_secs(20);
    let mut ready = false;
    let mut errors: Vec<SmokeError> = Vec::new();
    let mut detail: Vec<String> = Vec::new();

    while start.elapsed() < timeout {
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let (r, errs, det) = parse_smoke_log(&content);
            ready = r;
            errors = errs;
            detail = det;
        }
        if ready {
            // 再多等一会儿, 收集挂载后 (如 effect 内) 可能抛出的错误
            std::thread::sleep(Duration::from_millis(500));
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // 读取最终日志 (break 前可能还有写入), 再杀进程
    if let Ok(content) = std::fs::read_to_string(&log_path) {
        let (r, errs, det) = parse_smoke_log(&content);
        ready = r || ready;
        if !errs.is_empty() {
            errors = errs;
        }
        if !det.is_empty() {
            detail = det;
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&log_path);

    let ok = ready && errors.is_empty();
    SmokeReport {
        ok,
        ready,
        packaged: true,
        errors,
        detail: detail.join("\n"),
    }
}

/// 异步包装: 在 blocking 线程上跑 [`run_smoke_test`], 避免阻塞 async runtime.
///
/// 调用方无需直接依赖 tokio —— 本函数内部使用 `spawn_blocking`.
pub async fn run_smoke_test_async(app_dir: PathBuf, shell_path: PathBuf) -> SmokeReport {
    match tokio::task::spawn_blocking(move || run_smoke_test(&app_dir, &shell_path)).await {
        Ok(r) => r,
        Err(e) => SmokeReport {
            ok: false,
            ready: false,
            packaged: false,
            errors: vec![SmokeError {
                message: format!("冒烟测试任务失败: {e}"),
            }],
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_smoke_log_ready_and_error() {
        let content =
            "{\"level\":\"error\",\"message\":\"boom\"}\n{\"level\":\"ready\",\"message\":\"mounted\"}\n";
        let (ready, errors, detail) = parse_smoke_log(content);
        assert!(ready);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "boom");
        assert!(detail.iter().any(|l| l.contains("mounted")));
    }

    #[test]
    fn run_smoke_missing_dist_errors() {
        // 用一个不存在的 app_dir → dist 缺失, 不应启动任何进程
        let tmp = std::env::temp_dir()
            .join(format!("hermix_smoke_nonexist_{}", std::process::id()));
        let shell = tmp.join("shell.exe");
        let report = run_smoke_test(&tmp, &shell);
        assert!(!report.ok);
        assert!(!report.packaged);
        assert!(!report.errors.is_empty());
        assert!(report.errors[0].message.contains("未找到打包产物"));
    }
}
