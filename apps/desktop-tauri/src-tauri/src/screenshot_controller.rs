// 截图会话控制器：覆盖窗生命周期 + Tauri 命令。
// 数据通路见 screenshot.rs（抓屏/裁剪/剪贴板）与
// docs/notes/implemented/feature/2026-10-07-screenshot-tool.md。

use crate::screenshot::{self, Selection};
use base64::Engine as _;
use serde::Serialize;
use tauri::{Emitter, Listener, Manager};

const OVERLAY_LABEL: &str = "screenshot-overlay";

#[derive(Serialize, Clone)]
struct FramePayload {
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

/// 入口命令：抓屏 → 建覆盖窗 → 推送帧。可重入（已有覆盖窗则先关）。
#[tauri::command]
pub fn start_screenshot(app: tauri::AppHandle) -> Result<(), String> {
    // 重入：先关旧覆盖窗（正常路径不会出现，热键连按兜底）。
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }

    let session = screenshot::capture_monitor_at_cursor()?;

    // 幕布照片：全帧 BGRA→RGBA → PNG → data URL。
    let rgba = screenshot::bgra_to_rgba(&session.pixels);
    let png = screenshot::encode_png(&rgba, session.width, session.height)?;
    let data_url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png));

    // scale factor：Windows 主屏多数 1.0/1.25/1.5；取主窗为准（同屏显示）。
    let scale = app
        .get_webview_window("main")
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0);

    let payload = FramePayload {
        data_url,
        origin_x: session.origin_x,
        origin_y: session.origin_y,
        width: session.width,
        height: session.height,
        scale,
    };

    *screenshot::session().lock().unwrap() = Some(session);

    // 全屏覆盖窗：无边框、置顶、跳任务栏、不获焦（聚焦策略见下）。
    let (vx, vy, vw, vh) = virtual_monitor_bounds(&payload);
    let _webview = tauri::WebviewWindowBuilder::new(
        &app,
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
    .focused(true)
    .build()
    .map_err(|e| format!("覆盖窗创建失败: {e}"))?;

    // 帧数据在页面就绪后推（页面 emit "overlay-ready" 应答）。
    // 用 once 监听，避免每次会话叠加监听器。
    let app2 = app.clone();
    let payload2 = payload.clone();
    app.once("screenshot-overlay-ready", move |_event| {
        let _ = app2.emit("screenshot-frame", payload2);
    });

    Ok(())
}

/// 覆盖窗所在显示器的位置尺寸（虚拟屏幕坐标，物理像素）。
/// M1 范围：单显示器（光标所在屏），即缓存的位图范围本身。
fn virtual_monitor_bounds(payload: &FramePayload) -> (i32, i32, u32, u32) {
    (payload.origin_x, payload.origin_y, payload.width, payload.height)
}

/// 覆盖窗查询光标下窗口 rect（窗口模式 hover 高亮）。
/// 入参：光标逻辑坐标 + scale（覆盖窗端换算好的物理坐标直接传）。
#[tauri::command]
pub fn screenshot_window_at(x: i32, y: i32) -> Option<screenshot::WindowRect> {
    screenshot::window_rect_at((x, y))
}

/// 确认选区：裁剪 → 剪贴板 → 关覆盖窗。
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

    screenshot::copy_png_to_clipboard(&rgba, width, height)?;

    *screenshot::session().lock().unwrap() = None;
    close_overlay(&app);
    Ok(())
}

/// 取消：清会话、关覆盖窗。
#[tauri::command]
pub fn screenshot_cancel(app: tauri::AppHandle) -> Result<(), String> {
    *screenshot::session().lock().unwrap() = None;
    close_overlay(&app);
    Ok(())
}

fn close_overlay(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }
}
