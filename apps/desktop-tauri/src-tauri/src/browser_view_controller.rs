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
///
/// 由两个入口共用：标题栏 🌐（用户手动）与 stdio 桥的帧活动自动弹窗
/// （LLM 开始操作浏览器时小窗应自动现身——画面是给人看的，人不该还得
/// 自己去找开关）。
pub fn open_or_focus(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(VIEW_LABEL) {
        let focused = win.set_focus();
        eprintln!("[browser-view] window exists, focus result: {focused:?}");
        focused.map_err(|e| format!("聚焦浏览器小窗失败: {e}"))?;
        return Ok(());
    }
    let mut builder = WebviewWindowBuilder::new(
        app,
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
        .map_err(|e| {
            eprintln!("[browser-view] BUILD FAILED: {e}");
            format!("浏览器小窗创建失败: {e}")
        })?;
    eprintln!("[browser-view] window built ok");
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
    // 兜底 show：正常路径是前端 mount 后自调 show（先建后亮，白屏不可见）。
    // 但子窗侧任何 JS/权限故障都会让 show 永远不来——窗口"创建成功却永不
    // 出现"，用户侧症状是"点图标没反应"（2026-10-16 capabilities 漏配
    // browser-view 就是这么静默的）。5s 后仍不可见就 Rust 侧强制亮窗：
    // 小窗页面无启动屏（main.tsx 分支不拉主应用图），最坏闪一下占位 UI，
    // 远好于永远不可见。
    let fallback_app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let Some(win) = fallback_app.get_webview_window(VIEW_LABEL) else {
            return; // 窗口已被用户/会话结束路径关闭
        };
        if win.is_visible().unwrap_or(true) {
            return; // 前端已正常亮窗
        }
        tracing::warn!("浏览器小窗 5s 未亮窗（子窗页面可能加载失败），Rust 侧兜底 show");
        let _ = win.show();
        let _ = win.set_focus();
    });
    Ok(())
}

/// 标题栏 🌐 入口（用户手动）。
#[tauri::command]
pub async fn browser_view_open(app: tauri::AppHandle) -> Result<(), String> {
    eprintln!("[browser-view] open invoked");
    open_or_focus(&app)
}

/// stdio 桥帧活动回调（由 `stdio_bridge` 在**主线程**上调用）：一段新的
/// 浏览器帧流开始（距上一帧 ≥60s）时自动弹出小窗。突发（burst）内只在
 /// 开始时弹一次——用户中途关掉的窗口不会被逐帧反复拉起。
pub fn on_browser_frame_burst(app: &tauri::AppHandle) {
    if app.get_webview_window(VIEW_LABEL).is_some() {
        return; // 已开着（含最小化/被遮挡），不打扰
    }
    eprintln!("[browser-view] frame burst detected, auto-opening");
    if let Err(e) = open_or_focus(app) {
        tracing::warn!(error = %e, "浏览器小窗自动弹出失败");
    }
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
