//! 运行时冒烟测试 — 启动打包后的 hx-shell exe, 捕获启动错误.
//!
//! hx-shell 打包产物在 `dist/<id>.exe`。冒烟测试启动该 exe，
//! 检查进程是否能存活（不立即崩溃），并捕获 stdout/stderr 中的错误信息。
//! 供 agent 工具 (`custom_app_manager` 的 `smoke` action) 与桌面 Tauri 命令
//! (`custom_app_smoke_test`) 共用.

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
    /// 是否通过 (进程存活且无 stderr 错误)
    pub ok: bool,
    /// 进程是否成功启动并存活
    pub ready: bool,
    /// 是否已打包 (dist 存在)
    pub packaged: bool,
    /// 运行时错误列表
    pub errors: Vec<SmokeError>,
    /// 详细日志 (stdout + stderr)
    pub detail: String,
}

/// exe 在不同平台下的文件名.
fn dist_exe_name(id: &str) -> String {
    if cfg!(windows) {
        format!("{id}.exe")
    } else {
        id.to_string()
    }
}

/// 启动打包产物 (`dist/<id>.exe`) 并冒烟测试. 同步阻塞, 最多约 10s.
///
/// 调用方应先确保已打包. `app_dir` 为插件目录, 其下 `dist/` 必须有 `<id>.exe`.
pub fn run_smoke_test(app_dir: &Path) -> SmokeReport {
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

    let mut child = match Command::new(&exe_path)
        .current_dir(&dist_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return SmokeReport {
                ok: false,
                ready: false,
                packaged: true,
                errors: vec![SmokeError {
                    message: format!("启动应用失败: {e}"),
                }],
                detail: String::new(),
            };
        }
    };

    // 等待最多 8 秒, 检查进程是否存活
    let start = Instant::now();
    let timeout = Duration::from_secs(8);
    let mut ready = false;

    while start.elapsed() < timeout {
        // 检查进程是否已退出 (立即退出 = 崩溃)
        match child.try_wait() {
            Ok(Some(status)) => {
                // 进程已退出
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let (out_str, err_str) = capture_output(stdout, stderr);
                let detail = format!("stdout: {}\nstderr: {}", out_str, err_str);
                return SmokeReport {
                    ok: false,
                    ready: false,
                    packaged: true,
                    errors: vec![SmokeError {
                        message: format!("应用启动后立即退出 (exit={status})"),
                    }],
                    detail,
                };
            }
            Ok(None) => {
                // 进程仍在运行 = 启动成功
                if !ready {
                    ready = true;
                }
            }
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // 读取 stdout/stderr
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (out_str, err_str) = capture_output(stdout, stderr);

    // 杀掉进程
    let _ = child.kill();
    let _ = child.wait();

    let mut errors: Vec<SmokeError> = Vec::new();
    let mut detail = String::new();

    if !err_str.is_empty() {
        for line in err_str.lines().take(20) {
            let line = line.trim();
            if !line.is_empty() {
                errors.push(SmokeError {
                    message: line.to_string(),
                });
                detail.push_str("[stderr] ");
                detail.push_str(line);
                detail.push('\n');
            }
        }
    }

    if !out_str.is_empty() {
        for line in out_str.lines().take(10) {
            let line = line.trim();
            if !line.is_empty() {
                detail.push_str("[stdout] ");
                detail.push_str(line);
                detail.push('\n');
            }
        }
    }

    let ok = ready && errors.is_empty();
    SmokeReport {
        ok,
        ready,
        packaged: true,
        errors,
        detail,
    }
}

/// 从 stdout/stderr 管道中读取所有内容 (非阻塞).
fn capture_output(
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
) -> (String, String) {
    use std::io::Read;
    let out = stdout
        .map(|mut s| {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
        .unwrap_or_default();
    let err = stderr
        .map(|mut s| {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
        .unwrap_or_default();
    (out, err)
}

/// 异步包装: 在 blocking 线程上跑 [`run_smoke_test`], 避免阻塞 async runtime.
///
/// 调用方无需直接依赖 tokio —— 本函数内部使用 `spawn_blocking`.
pub async fn run_smoke_test_async(app_dir: PathBuf) -> SmokeReport {
    match tokio::task::spawn_blocking(move || run_smoke_test(&app_dir)).await {
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
    fn run_smoke_missing_dist_errors() {
        // 用一个不存在的 app_dir → dist 缺失, 不应启动任何进程
        let tmp = std::env::temp_dir()
            .join(format!("hermix_smoke_nonexist_{}", std::process::id()));
        let report = run_smoke_test(&tmp);
        assert!(!report.ok);
        assert!(!report.packaged);
        assert!(!report.errors.is_empty());
        assert!(report.errors[0].message.contains("未找到打包产物"));
    }
}
