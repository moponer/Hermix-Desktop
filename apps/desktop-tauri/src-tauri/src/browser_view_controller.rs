//! 浏览器小窗控制器：把无头 Edge 的 screencast 帧流渲染成一个独立 Tauri
//! 子窗（label `browser-view`）。
//!
//! 范式完全复用 `screenshot_controller.rs`：运行时 `WebviewWindowBuilder`
//! 建窗、`incognito(true)` 对齐主窗、`visible(false)` 先建后亮（前端 mount
//! 完自调 `getCurrentWindow().show()`，白屏不可能被看到）。
//!
//! 与截图覆盖窗的关键差异：
//!
//! - **窗口与浏览器会话生命周期解耦**：关小窗 ≠ 关 Edge。会话是
//!   hermix-tools 侧的静态单例，只死于 `browser_close` 工具或进程退出
//!   （Job Object 兜底）。关窗后 pump 空转，重开即最新帧（broadcast 丢旧帧）。
//! - **帧流不落盘**：receiver → base64 → `emit_to` 直达子窗，全程内存。
//!   唯一写盘是用户在子窗里点「保存当前帧」——那是前端 canvas 的事。
//! - **暂停只停 Rust→前端推流**（`PAUSED`），不停 Chrome 侧 screencast：
//!   ego-browser 实测教训——采集后端停流后窗口恢复时反复重建会画面抖动。
//!   彻底停编码（`Page.stopScreencast`）留待实测 Edge CPU 占比后再决定。
//! - **几何记忆**：关闭时把 position + inner_size 落
//!   `<hermix_home>/browser-view-geometry.json`，重开还原。

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const VIEW_LABEL: &str = "browser-view";
const VIEW_ROUTE: &str = "index.html#/browser-view";
const GEOMETRY_FILE: &str = "browser-view-geometry.json";
/// 无历史几何时的默认尺寸（逻辑像素）。
const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 400.0;
/// pump 轮询 receiver 的间隔：会话未建立 / 已关闭时 reactor 空转的节奏。
const PUMP_POLL_MS: u64 = 300;

/// 子窗的 WebView2 环境参数。必须与 tauri.conf.json 主窗口的
/// additionalBrowserArgs **逐字一致**：同一 user data folder 下所有 webview
/// 共享一个浏览器进程，环境级选项不一致时创建直接报
/// 0x8007139F（ERROR_INVALID_STATE）——截图覆盖窗踩过这个坑。
const VIEW_BROWSER_ARGS: &str = "--disable-features=msSmartScreenProtection,EdgeFeed,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling,RendererCodeIntegrity --disable-renderer-backgrounding --disable-background-timer-throttling --disk-cache-size=20971520 --js-flags=--max-old-space-size=1024";

/// 前端上报的暂停/恢复入口：小窗不可见时**真停 Chrome 侧 screencast 编码**
/// （评审修正 1：只停 Rust→前端推流是未实测就抄的退化路径，动画页 Edge
/// 照常拉满）。Rust 侧仍保留 PAUSED 拦截（pause 生效前最后一两帧不推），
/// 双保险：Chrome 停编码是 CPU 闸门，PAUSED 是推流闸门。
static PAUSED: AtomicBool = AtomicBool::new(false);

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
    // 系统标题栏：拖动 / 缩放 / 最大化白拿，自定义标题栏留后续美化。
    .decorations(true)
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

/// 前端暂停/恢复上报：小窗不可见时真停 Chrome 侧 screencast 编码
/// （`Page.stopScreencast`），恢复时对同一页面重发 start——CDP restart
/// 成本毫秒级；若真机出现 ego 式恢复抖动，退化路径是只降
/// every_nth_frame 不停流（见 implemented 笔记 P0 评审修正 1）。
#[tauri::command]
pub async fn browser_view_set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::Relaxed);
    if paused {
        hermix_tools::tools::os_auto::browser::screencast::pause().await;
    } else {
        hermix_tools::tools::os_auto::browser::screencast::resume().await;
    }
}

/// setup 初始化：起帧泵（会话未建立时空转轮询 receiver，session 建起即
/// 自动开始推流）。窗口几何记忆的监听在建窗时注册（见 browser_view_open）。
/// 在 lib.rs 的 `.setup(|app| …)` 里调用一次。
pub fn init(app: &tauri::AppHandle) {
    // 帧泵常驻：会话未建立时空转轮询 receiver，session 建起即自动开始推流。
    let pump_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let Some(mut rx) =
                hermix_tools::tools::os_auto::browser::screencast::receiver()
            else {
                tokio::time::sleep(std::time::Duration::from_millis(PUMP_POLL_MS)).await;
                continue;
            };
            // 有会话：收到帧就推。窗口不在 / 暂停时不推（帧照收，广播丢旧帧）。
            while let Ok(frame) = rx.recv().await {
                if PAUSED.load(Ordering::Relaxed) {
                    continue;
                }
                if pump_app.get_webview_window(VIEW_LABEL).is_none() {
                    continue;
                }
                let payload = BrowserFrame {
                    data: format!(
                        "data:image/jpeg;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&*frame)
                    ),
                    bytes: frame.len(),
                };
                // 子窗已关 / 事件目标不存在时 emit 失败——无接收者时零成本。
                let _ = pump_app.emit_to(VIEW_LABEL, "browser-frame", payload);
            }
            // 事件流结束 = 会话关闭。回到轮询等下一个 session。
        }
    });
}

#[derive(Serialize, Clone)]
struct BrowserFrame {
    /// `data:image/jpeg;base64,…`，前端直接喂 `Image` / `createImageBitmap`。
    data: String,
    /// 原始字节数（标题栏显示帧大小用）。
    bytes: usize,
}
