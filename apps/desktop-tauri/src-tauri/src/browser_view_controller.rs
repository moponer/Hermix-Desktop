//! 浏览器小窗控制器：把无头 Edge 的 screencast 帧流渲染成一个独立 Tauri
//! 子窗（label `browser-view`）。
//!
//! 范式复用 `screenshot_controller.rs`：运行时 `WebviewWindowBuilder` 建窗、
//! `incognito(true)` 对齐主窗、`visible(false)` 先建后亮（前端 mount 完自调
//! `getCurrentWindow().show()`，白屏不可能被看到）。
//!
//! 与截图覆盖窗的关键差异：
//!
//! - **窗口与浏览器会话生命周期解耦**：关小窗 ≠ 关 Edge。会话是 sidecar
//!   进程（hermix.exe）里的静态单例，只死于 `browser_close` 工具或进程退出。
//!   关窗后重开即自动接上最新帧。
//! - **帧流跨进程**：浏览器会话跑在 sidecar，桌面进程的 `screencast::receiver()`
//!   永远为 None（`static` 是进程内单例，不跨进程）。帧由 sidecar 经 stdout
//!   以 `channel="browser-frame"` 推过来，桌面端 `stdio_bridge` 转成
//!   `stdio-frame` 事件，前端 `BrowserView.tsx` 直接 `listen("stdio-frame")`
//!   按 channel 过滤收帧——本控制器不再持有泵，只管建窗与几何记忆。
//! - **暂停只停 sidecar 的 Chrome 编码**：小窗隐藏时前端把指令经 stdio 桥
//!   `POST /api/browser-view/paused` 发到 sidecar（评审修正：此前直接调桌面侧
//!   `screencast::pause()` 是静默 no-op——会话根本不在桌面进程）。前端同时
//!   不再渲染新帧（pausedRef 闸门），双保险。
//! - **几何记忆**：关闭时把 position + inner_size 落
//!   `<hermix_home>/browser-view-geometry.json`，重开还原。

use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

const VIEW_LABEL: &str = "browser-view";
const VIEW_ROUTE: &str = "index.html#/browser-view";
const GEOMETRY_FILE: &str = "browser-view-geometry.json";
/// 无历史几何时的默认尺寸（逻辑像素）。
const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 400.0;

/// 子窗的 WebView2 环境参数。必须与 tauri.conf.json 主窗口的
/// additionalBrowserArgs **逐字一致**：同一 user data folder 下所有 webview
/// 共享一个浏览器进程，环境级选项不一致时创建直接报
/// 0x8007139F（ERROR_INVALID_STATE）——截图覆盖窗踩过这个坑。
const VIEW_BROWSER_ARGS: &str = "--disable-features=msSmartScreenProtection,EdgeFeed,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling,RendererCodeIntegrity --disable-renderer-backgrounding --disable-background-timer-throttling --disk-cache-size=20971520 --js-flags=--max-old-space-size=1024";

#[derive(Serialize, Deserialize)]
struct Geometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn geometry_path() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join(GEOMETRY_FILE)
}

fn load_geometry() -> Option<Geometry> {
    let raw = std::fs::read_to_string(geometry_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_geometry(app: &tauri::AppHandle) {
    let Some(win) = app.get_webview_window(VIEW_LABEL) else {
        return;
    };
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) else {
        return;
    };
    let geom = Geometry {
        x: pos.x as f64,
        y: pos.y as f64,
        width: size.width as f64,
        height: size.height as f64,
    };
    if let Ok(json) = serde_json::to_string(&geom) {
        if let Err(e) = std::fs::write(geometry_path(), json) {
            tracing::debug!(error = %e, "浏览器小窗几何保存失败（下次用默认位置）");
        }
    }
}

/// 打开（或聚焦）浏览器小窗。幂等：已开则只聚焦。
#[tauri::command]
pub async fn browser_view_open(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(VIEW_LABEL) {
        win.set_focus().map_err(|e| format!("聚焦浏览器小窗失败: {e}"))?;
        return Ok(());
    }
    let mut builder = WebviewWindowBuilder::new(
        &app,
        VIEW_LABEL,
        WebviewUrl::App(VIEW_ROUTE.into()),
    )
    .title("浏览器小窗")
    .additional_browser_args(VIEW_BROWSER_ARGS)
    // 与主窗对齐：画面内容不该落进任何持久化 profile。
    .incognito(true)
    .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
    // 自定义标题栏由前端 `.bv-bar` 提供（拖拽 / 最大化 / 关闭 / 状态）。
    // 不带系统标题栏，否则系统栏的图标与 .bv-bar 叠成双层（标题栏图标 bug）。
    .decorations(false)
    // 先建后亮：前端 mount 完自调 show，白屏不可能被看到。
    .visible(false);

    if let Some(g) = load_geometry() {
        builder = builder.position(g.x, g.y).inner_size(g.width, g.height);
    }

    builder
        .build()
        .map_err(|e| format!("浏览器小窗创建失败: {e}"))?;
    // 几何记忆注册在这一刻（WebviewWindow 的 on_window_event，不是 AppHandle
    // ——后者没有这个 API）。窗口销毁后监听自然失效，下次 open 重建时再注册。
    if let Some(win) = app.get_webview_window(VIEW_LABEL) {
        let geo_app = app.clone();
        win.on_window_event(move |event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                save_geometry(&geo_app);
            }
        });
    }
    tracing::info!("浏览器小窗已创建（{VIEW_LABEL}）");
    Ok(())
}

/// 前端上报暂停/恢复：小窗最小化/遮挡时停 sidecar 里 headless Edge 的
/// screencast 编码（CPU 闸门）。浏览器会话在 sidecar 进程，桌面进程自己的
/// `screencast::pause()` 永远是空操作——指令必须经 stdio 桥发到 sidecar 才
/// 真生效（评审修正：此前直接调桌面侧 pause 是静默 no-op）。
#[tauri::command]
pub async fn browser_view_set_paused(paused: bool) {
    if let Some(bridge) = crate::stdio_bridge::global() {
        let body = serde_json::json!({ "paused": paused }).to_string();
        let _ = bridge
            .http_request("POST", "/api/browser-view/paused", Some(&body))
            .await;
    }
}
