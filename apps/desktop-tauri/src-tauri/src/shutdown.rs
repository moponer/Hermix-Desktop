//! 进程退出的**唯一**清理收口（2026-10-27）。
//!
//! 此前清理逻辑散在三处且互不覆盖：窗口 `Destroyed` 只停 sidecar、托盘
//! 「退出」自己抄了一份 sidecar + filesearch 停止请求、`RunEvent` 完全没接。
//! 漏掉的路径全都真实存在：`app.exit()` 不触发窗口 `Destroyed`、任务管理器
//! 结束、注销 / 关机、panic 退出。残留的 `hermix.exe` 占住 `sessions.db` 与
//! `binaries/` 里的 exe（编译 EBUSY）、`hermix-filesearch.exe` 是提权进程且
//! 注册了开机计划任务，taskkill 连管理员都不一定杀得动。
//!
//! 现在只有一条路径：`cleanup()`。幂等，由 `RunEvent::ExitRequested` /
//! `RunEvent::Exit` / 托盘退出 / 主窗销毁共同调用。
//!
//! **不杀 `hermix-viewer.exe`**：它是独立文件查看器，用户关掉主程序后继续
//! 看文件是正常用法；它自己的窗口关掉即退出，残留只来自用户没关窗口。

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;

static CLEANED: AtomicBool = AtomicBool::new(false);

/// 停掉本程序拉起的全部后台子进程。可重复调用，只有第一次真正执行。
pub fn cleanup(app: &tauri::AppHandle) {
    if CLEANED.swap(true, Ordering::SeqCst) {
        return;
    }

    // 1) hermix.exe（stdio sidecar）：先发 shutdown 帧走优雅退出，3s 超时
    //    强杀；`stop()` 一并释放 Job Object，孙进程（node / python / 浏览器）
    //    随作业树死。
    if let Some(state) = app.try_state::<crate::AppState>() {
        match state.sidecar.lock() {
            Ok(mut sb) => sb.stop(),
            // 锁中毒也要保证桥被标记为死亡，否则前端还会往死进程写帧。
            Err(_) => crate::stdio_bridge::mark_dead(),
        }
    }

    // 2) hermix-filesearch.exe：提权进程，宿主侧 TerminateProcess 会被
    //    「拒绝访问」挡回来，唯一正道是写停止请求让它的看门狗自己走
    //    （10s 一轮 → 刷盘 → 删 endpoint.json → 退出）。
    request_filesearch_stop();
}

/// 写索引守护进程的停止请求。目录不存在 = 从未启动过，不创建（不留垃圾）。
fn request_filesearch_stop() {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = std::path::PathBuf::from(local)
        .join("hermix")
        .join("filesearch");
    if !dir.is_dir() {
        return;
    }
    let body = format!(
        "{{\"requestedAt\":{}}}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    if let Err(e) = std::fs::write(dir.join("stop-request.json"), body) {
        eprintln!("[Hermix] 写索引服务停止请求失败: {e}");
    }
}
