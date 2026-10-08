// 截图会话控制器：覆盖窗生命周期 + Tauri 命令。
// 数据通路见 screenshot.rs（抓屏/裁剪/剪贴板）与
// docs/notes/implemented/feature/2026-10-07-screenshot-tool.md。
//
// 帧下发走 invoke 拉取（screenshot_frame_data）而非事件推送：多 MB 的
// data URL 经事件通道要过 JSON eval 注入，易碎且有一次监听竞态；帧在
// 建窗前就已入静态缓存，前端挂载后直接拉，无时序耦合。

use crate::screenshot::{self, Selection};
use base64::Engine as _;
use serde::Serialize;
use tauri::{Emitter, Manager};

const OVERLAY_LABEL: &str = "screenshot-overlay";

#[derive(Serialize, Clone)]
pub struct FramePayload {
    /// 全屏照片的 data URL（PNG），覆盖窗直接铺 <img>
    data_url: String,
    /// 位图原点（虚拟屏幕坐标）——覆盖窗把本地坐标换算回去用
    origin_x: i32,
    origin_y: i32,
    /// 物理像素尺寸（= 覆盖窗逻辑尺寸 × scale factor）
    width: u32,
    height: u32,
    /// 显示器 scale factor（光标坐标逻辑→物理换算）
    scale: f64,
}

static FRAME: once_cell::sync::OnceCell<std::sync::Mutex<Option<FramePayload>>> =
    once_cell::sync::OnceCell::new();

fn frame() -> &'static std::sync::Mutex<Option<FramePayload>> {
    FRAME.get_or_init(|| std::sync::Mutex::new(None))
}

/// 入口命令：异步包装——重活放阻塞线程池，主线程零阻塞。
#[tauri::command]
pub async fn start_screenshot(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || run_session(&app))
        .await
        .map_err(|e| format!("截图任务调度失败: {e}"))?
}

/// 会话主流程（重活，走 spawn_blocking 调用——抓屏 + PNG 编码数秒，
/// 同步命令跑主线程会冻结整个应用 UI）。
pub fn run_session(app: &tauri::AppHandle) -> Result<(), String> {
    // 重入：先关旧覆盖窗（正常路径不会出现，热键连按兜底）。
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }

    let session = screenshot::capture_monitor_at_cursor()?;

    // 幕布照片：全帧 BGRA→RGBA → PNG → data URL。
    let rgba = screenshot::bgra_to_rgba(&session.pixels);
    let png = screenshot::encode_png(&rgba, session.width, session.height)?;
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );

    let payload = FramePayload {
        data_url,
        origin_x: session.origin_x,
        origin_y: session.origin_y,
        width: session.width,
        height: session.height,
        scale: session.scale,
    };
    let (vx, vy, vw, vh) = (payload.origin_x, payload.origin_y, payload.width, payload.height);
    let scale = payload.scale;

    *screenshot::session().lock().unwrap() = Some(session);
    *frame().lock().unwrap() = Some(payload);

    // 全屏覆盖窗：无边框、置顶、跳任务栏。visible(false) 先建后亮——
    // 页面加载完成、帧就绪后由前端 show()，杜绝"白窗先闪再变画面"。
    // M1 范围：单显示器（光标所在屏），builder.position 为逻辑坐标，
    // 主屏原点 (0,0) 下与虚拟屏坐标一致；多屏混排 DPI 留 M3。
    let built = tauri::WebviewWindowBuilder::new(
        app,
        OVERLAY_LABEL,
        tauri::WebviewUrl::App("index.html#/screenshot-overlay".into()),
    )
    .title("Screenshot")
    .position(vx as f64, vy as f64)
    .inner_size(vw as f64 / scale, vh as f64 / scale)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false)
    .focused(true)
    .build();

    if let Err(e) = built {
        *screenshot::session().lock().unwrap() = None;
        *frame().lock().unwrap() = None;
        return Err(format!("覆盖窗创建失败: {e}"));
    }

    spawn_esc_watcher(app.clone());

    // 兜底 show：前端拉帧失败/页面加载失败时窗口不能永远隐形。
    let fallback = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Some(win) = fallback.get_webview_window(OVERLAY_LABEL) {
            let _ = win.show();
        }
    });

    Ok(())
}

/// ESC 兜底监视线程：GetAsyncKeyState 是系统级按键状态，不依赖覆盖窗
/// webview 是否持有键盘焦点（热键触发后焦点不保证落进覆盖窗，前端
/// keydown 收不到 ESC 的场景由此路径兜住）。边沿触发，松开一次才开始
/// 监听，避免"建窗前按下的 ESC"在首帧就把会话取消掉。
fn spawn_esc_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // 等待 ESC 松开（上限 2s），再进入边沿检测。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while screenshot::esc_pressed() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let mut prev = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            if app.get_webview_window(OVERLAY_LABEL).is_none() {
                break; // 会话已结束（确认/取消/兜底关闭）
            }
            let pressed = screenshot::esc_pressed();
            if pressed && !prev {
                cancel_session(&app);
                break;
            }
            prev = pressed;
        }
    });
}

fn cancel_session(app: &tauri::AppHandle) {
    *screenshot::session().lock().unwrap() = None;
    *frame().lock().unwrap() = None;
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }
}

/// 覆盖窗拉取幕布帧（数据在会话建立时就已缓存，无时序耦合）。
#[tauri::command]
pub fn screenshot_frame_data() -> Option<FramePayload> {
    frame().lock().unwrap().clone()
}

/// 覆盖窗查询光标下窗口 rect（窗口模式 hover 高亮）。
/// 入参：物理坐标（覆盖窗端换算好）。排除覆盖窗自身——全屏置顶窗
/// 不排除的话任何命中都是覆盖窗自己，窗口模式永远高亮整屏。
#[tauri::command]
pub fn screenshot_window_at(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
) -> Option<screenshot::WindowRect> {
    let exclude = app
        .get_webview_window(OVERLAY_LABEL)
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    screenshot::window_rect_at((x, y), exclude)
}

/// 确认选区：裁剪 → 剪贴板 → 关覆盖窗。失败要可见（主窗 toast）。
#[tauri::command]
pub fn screenshot_confirm(app: tauri::AppHandle, selection: Selection) -> Result<(), String> {
    let session_guard = screenshot::session().lock().unwrap();
    let Some(session) = session_guard.as_ref() else {
        return Err("截图会话已过期".into());
    };
    let Some(rgba) = screenshot::crop_rgba(session, selection) else {
        return Err("选区超出屏幕范围".into());
    };
    let width = selection.width.max(0) as u32;
    let height = selection.height.max(0) as u32;
    drop(session_guard);

    match screenshot::copy_png_to_clipboard(&rgba, width, height) {
        Ok(()) => {
            *screenshot::session().lock().unwrap() = None;
            *frame().lock().unwrap() = None;
            close_overlay(&app);
            let _ = app.emit("hermix:screenshot-copied", ());
            Ok(())
        }
        Err(e) => {
            // 剪贴板被占用等失败：覆盖窗留在原地，用户可重试或 ESC。
            let _ = app.emit("hermix:screenshot-error", e.clone());
            Err(e)
        }
    }
}

/// 取消：清会话、关覆盖窗。
#[tauri::command]
pub fn screenshot_cancel(app: tauri::AppHandle) -> Result<(), String> {
    cancel_session(&app);
    Ok(())
}

fn close_overlay(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }
}
