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
use std::time::{Duration, Instant};

use tauri::Manager;

static CLEANED: AtomicBool = AtomicBool::new(false);

/// 退出流程是否已经启动。watchdog 必须在它之后收工 —— 只停 sidecar 不停
/// 进程时，看护逻辑会把刚停掉的子进程当故障再拉起来（2026-10-30 实锤）。
pub fn is_cleaned() -> bool {
    CLEANED.load(Ordering::SeqCst)
}

/// 停掉本程序拉起的全部后台子进程。可重复调用，只有第一次真正执行。
///
/// `trigger` 只用于日志：本函数会发 shutdown 帧停掉 sidecar，而这是 server.log
/// 里唯一能看到的现象（`stdio: shutdown frame received`）。2026-10-29 排查
/// 「后端自己断了」时，外壳侧没有任何记录能说明是退出流程还是别处停了它，
/// 只能靠反推 —— 所以每个调用点都必须自报家门。
pub fn cleanup(app: &tauri::AppHandle, trigger: &str) {
    if CLEANED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::desktop_log!("[Hermix] cleanup: stopping sidecar (trigger={trigger})");

    // 1) hermix.exe（stdio sidecar）：先发 shutdown 帧走优雅退出，超时
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
    //    （1s 一轮 → 刷盘 → 退出）。
    request_filesearch_stop();
    wait_filesearch_gone(Duration::from_millis(2000));
}

/// 收尾 + 请求退出进程。主窗销毁、僵尸实例被唤起两条路径共用。
///
/// **只 cleanup 不 exit 会留下僵尸**：进程没有可见窗口却还活着，继续持有
/// 单实例锁与 sidecar（watchdog 会把它拉回来），用户再点图标只会静默
/// 退出（`single_instance` 认为已有实例在跑）。2026-10-30 实锤见
/// `docs/notes/implemented/bug-fix/2026-10-30-close-leaves-zombie-process.md`。
pub fn cleanup_and_exit(app: &tauri::AppHandle, trigger: &str) {
    cleanup(app, trigger);
    app.exit(0);
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
        crate::desktop_log!("[Hermix] 写索引服务停止请求失败: {e}");
    }
}

/// 等索引守护进程真的退掉（有界）。只写停止请求就 `exit(0)` 的话，用户
/// 紧接着重开主程序或跑安装器时会撞上它——它还活着就锁着
/// `hermix-filesearch.exe`（编译 EBUSY、安装器报"文件被占用"）。
///
/// 判据用回环端口而不是 `OpenProcess`：它是提权进程，普通权限连句柄都
/// 拿不到；进程一退端口立即释放。读不到 `endpoint.json` = 从未启动过，
/// 直接返回。
fn wait_filesearch_gone(budget: Duration) {
    let Some(port) = filesearch_port() else {
        return;
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    crate::desktop_log!("[Hermix] 索引服务 {budget:?} 内未退出（多半在刷盘，稍后自行结束）");
}

fn filesearch_port() -> Option<u16> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let raw = std::fs::read_to_string(
        std::path::PathBuf::from(local)
            .join("hermix")
            .join("filesearch")
            .join("endpoint.json"),
    )
    .ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()?
        .get("port")?
        .as_u64()
        .map(|port| port as u16)
}
