//! 钉住的预览浮窗（重量模式）。
//!
//! **为什么要有**：用户要「把某个目录 / 某个文件的预览钉在屏幕某个位置，
//! 多个窗口不用来回切，方便对比」。这是文件查看器的经典诉求（PowerToys
//! 的 Peek / VS Code 的拖出浮窗都是同一语义）。
//!
//! **三条硬约束**（都来自 2026-10-21 的调研，不是拍脑袋）：
//!
//! 1. **每个浮窗是独立的 JS realm**。Tauri 的每个 `WebviewWindow` 都是
//!    一个独立 WebView2 环境，模块级变量不共享（Obsidian 官方文档明确：
//!    pop-out 有自己的 `Window`/`Document`/全套全局构造器）。所以浮窗
//!    **不共享前端状态**——共享的东西一律放 Rust 侧，用 `emit_to(label)`
//!    定向投递。
//! 2. **浮窗必须是只读预览**。主窗已经在编辑同一个文件了，浮窗再来一份
//!    可编辑的 = 两个实例写同一个文件 = 互相覆盖。这是最容易埋的坑。
//! 3. **位置要记住"显示器是谁"**。便携应用拔着 U 盘换机器是常态，显示器
//!    拔了以后按旧坐标恢复 = 窗口跑到屏幕外。恢复时若原屏不在，就落到
//!    主屏的 work area（工作区，不含任务栏）里。
//!
//! **坐标语义**：Rust 侧的 `PhysicalPosition` / `PhysicalSize` 是**物理
//! 像素**；`Monitor::position()` 也是物理像素。浮窗位置一律存物理像素
//! （i32，允许负坐标 —— 副屏在主屏左边时 x 为负）。
//!
//! 产物与窗口都不是"文件"，没有原文件保护问题；但**产物名**要跟着源路径
//! 走，见 `pin_key`。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{
    LogicalPosition, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use hermix_config::paths::hermix_home;

/// 浮窗 label 前缀。前端据此判断"我是浮窗"，渲染轻量入口而不是完整 App。
pub const PIN_PREFIX: &str = "pin-";

/// 同时最多几个浮窗。每个浮窗 = 一个独立 WebView2 实例，内存线性增长，
/// 必须有上限（超限时关掉最旧的那个）。
pub const MAX_PINS: usize = 6;

/// 浮窗默认尺寸（物理像素，按 1.0 缩放估的；真机会被 DPI 修正）。
const DEFAULT_W: u32 = 900;
const DEFAULT_H: u32 = 640;

/// 一个浮窗的落位信息。存**物理像素** + 显示器身份。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PinPlacement {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub always_on_top: bool,
    /// 显示器标识（`Monitor::name()`）：拔屏后据此判断"原来那块屏不在了"。
    pub monitor: String,
    /// 当时的缩放因子。DPI 变了说明换机器了，旧坐标没意义。
    pub scale: f64,
}

/// 浮窗的对外信息（给前端列表用）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PinInfo {
    pub label: String,
    /// 源路径（文件或目录）。
    pub path: String,
    pub always_on_top: bool,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 预设落位（菜单里的「钉到…」八宫格）。
#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PinPreset {
    pub key: &'static str,
    pub label: &'static str,
}

/// 落位记忆文件：`$hermix_home/pinned-windows.json`。
fn placement_path() -> PathBuf {
    hermix_home().join("pinned-windows.json")
}

fn load_placements() -> BTreeMap<String, PinPlacement> {
    let p = placement_path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_placements(map: &BTreeMap<String, PinPlacement>) -> Result<(), String> {
    let p = placement_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = serde_json::to_string_pretty(map).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(&p, text).map_err(|e| format!("写入落位记忆失败: {e}"))
}

/// 浮窗的键：源路径。同一路径重复钉 = 复用同一个浮窗（不刷出一堆）。
fn pin_key(path: &str) -> String {
    path.to_string()
}

/// 百分比编码（路径里的 `\ : 空格 # ?` 都不能裸奔进 query）。
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 当前的浮窗列表（按创建顺序）。
fn pin_windows(app: &tauri::AppHandle) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (label, w) in app.webview_windows() {
        if label.starts_with(PIN_PREFIX) {
            // 路径从窗口 URL 的 query 里取回来（前端也读同一个参数）。
            let path = w
                .url()
                .ok()
                .and_then(|u| u.query().map(|q| q.to_string()))
                .and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("pin="))
                        .map(|v| percent_decode(v))
                })
                .unwrap_or_default();
            out.push((label, path));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 落位恢复：原显示器不在了（或 DPI 变了）就落到主屏工作区。
///
/// `work_area` 是**工作区**（不含任务栏）—— 用 `size` 会被任务栏压住。
fn resolve_placement(
    app: &tauri::AppHandle,
    saved: Option<&PinPlacement>,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let default = (PhysicalPosition::new(80, 80), PhysicalSize::new(DEFAULT_W, DEFAULT_H));
    let Some(saved) = saved else {
        return default;
    };
    let monitors = app.available_monitors().unwrap_or_default();
    // 原屏还在**且** DPI 没变 → 用原屏；否则用主屏（DPI 变了说明换机器了，
    // 旧坐标的视觉位置已经不是那个位置）。
    let chosen = monitors
        .iter()
        .find(|m| m.name() == Some(&saved.monitor) && (m.scale_factor() - saved.scale).abs() < 0.01);
    let primary = app.primary_monitor().unwrap_or(None);
    let monitor: &tauri::Monitor = match chosen {
        Some(m) => m,
        None => match primary.as_ref() {
            Some(m) => m,
            None => return default,
        },
    };
    let work = monitor.work_area();
    let w = saved.w.clamp(240, work.size.width);
    let h = saved.h.clamp(200, work.size.height);
    // 夹在工作区里：窗口不能跑到屏幕外，也不能压在任务栏下。
    let max_x = work.position.x + work.size.width as i32 - w as i32;
    let max_y = work.position.y + work.size.height as i32 - h as i32;
    let x = saved.x.clamp(work.position.x, max_x.max(work.position.x));
    let y = saved.y.clamp(work.position.y, max_y.max(work.position.y));
    (PhysicalPosition::new(x, y), PhysicalSize::new(w, h))
}

/// 打开一个钉住的预览浮窗，返回它的 label。
///
/// `path` 可以是文件（渲染预览）或目录（渲染列表）。同一路径重复调用 =
/// 复用既有浮窗并把它带到前面。
#[tauri::command]
pub fn viewer_pin_open(app: tauri::AppHandle, path: String) -> Result<String, String> {
    let src = PathBuf::from(&path);
    if !src.exists() {
        return Err(format!("路径不存在: {path}"));
    }
    // 同一路径已有浮窗 → 聚焦它，不新开。
    for (label, p) in pin_windows(&app) {
        if p == path {
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.unminimize();
                let _ = w.set_focus();
                return Ok(label);
            }
        }
    }
    let pins = pin_windows(&app);
    if pins.len() >= MAX_PINS {
        // 超限：关掉最旧的那个（label 后缀是递增序号，最小即最旧）。
        if let Some(oldest) = pins
            .iter()
            .min_by_key(|(label, _)| label[PIN_PREFIX.len()..].parse::<u64>().unwrap_or(0))
            .map(|(label, _)| label.clone())
        {
            if let Some(w) = app.get_webview_window(&oldest) {
                let _ = w.close();
            }
        }
    }
    let seq = next_seq(&app);
    let label = format!("{PIN_PREFIX}{seq}");
    let placements = load_placements();
    let saved = placements.get(&pin_key(&path)).cloned();
    let (pos, size) = resolve_placement(&app, saved.as_ref());
    let top = saved.map(|s| s.always_on_top).unwrap_or(false);

    let url = format!("index.html?pin={}", encode_query(&path));
    let builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(title_for(&src))
        .inner_size(size.width as f64, size.height as f64)
        .position(pos.x as f64, pos.y as f64)
        .always_on_top(top)
        .resizable(true);
    let window = builder
        .build()
        .map_err(|e| format!("创建浮窗失败: {e}"))?;

    // 移动/缩放后把新位置写回记忆（浮窗自己记，不用前端转手）。
    let app2 = app.clone();
    let key = pin_key(&path);
    let label2 = label.clone();
    window.on_window_event(move |event| {
        use tauri::WindowEvent;
        match event {
            WindowEvent::Moved(p) => {
                let mut m = load_placements();
                let e = m.entry(key.clone()).or_default();
                e.x = p.x;
                e.y = p.y;
                e.monitor = monitor_name(&app2, &label2);
                e.scale = window_scale(&app2, &label2);
                let _ = save_placements(&m);
            }
            WindowEvent::Resized(s) => {
                let mut m = load_placements();
                let e = m.entry(key.clone()).or_default();
                e.w = s.width;
                e.h = s.height;
                let _ = save_placements(&m);
            }
            _ => {}
        }
    });

    Ok(label)
}

/// 浮窗序号（label 后缀）。
fn next_seq(app: &tauri::AppHandle) -> u64 {
    let mut max = 0u64;
    for (label, _) in pin_windows(app) {
        if let Ok(v) = label[PIN_PREFIX.len()..].parse::<u64>() {
            max = max.max(v);
        }
    }
    max + 1
}

fn window_scale(app: &tauri::AppHandle, label: &str) -> f64 {
    app.get_webview_window(label)
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0)
}

fn monitor_name(app: &tauri::AppHandle, label: &str) -> String {
    app.get_webview_window(label)
        .and_then(|w| w.current_monitor().ok().flatten())
        .and_then(|m| m.name().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// 浮窗标题：文件名 + 「· Hermix」。
fn title_for(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    format!("{name} · Hermix")
}

/// 关闭浮窗。
#[tauri::command]
pub fn viewer_pin_close(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.close();
    }
    Ok(())
}

/// 浮窗列表。
#[tauri::command]
pub fn viewer_pin_list(app: tauri::AppHandle) -> Vec<PinInfo> {
    pin_windows(&app)
        .into_iter()
        .map(|(label, path)| {
            let (x, y, w, h, top) = app
                .get_webview_window(&label)
                .map(|win| {
                    let p = win.outer_position().unwrap_or(PhysicalPosition::new(0, 0));
                    let s = win.outer_size().unwrap_or(PhysicalSize::new(0, 0));
                    let t = win.is_always_on_top().unwrap_or(false);
                    (p.x, p.y, s.width, s.height, t)
                })
                .unwrap_or((0, 0, 0, 0, false));
            PinInfo {
                label,
                path,
                always_on_top: top,
                x,
                y,
                w,
                h,
            }
        })
        .collect()
}

/// 浮窗置顶开关。
#[tauri::command]
pub fn viewer_pin_set_top(
    app: tauri::AppHandle,
    label: String,
    top: bool,
) -> Result<(), String> {
    let Some(w) = app.get_webview_window(&label) else {
        return Err("浮窗已关闭".into());
    };
    w.set_always_on_top(top).map_err(|e| format!("设置置顶失败: {e}"))?;
    Ok(())
}

/// 把浮窗挪到预设位置（八宫格 + 居中）。
///
/// 位置按**逻辑像素**给 Tauri（`LogicalPosition`），它会按目标显示器
/// 的缩放换算成物理像素 —— 直接给物理像素会在 150% 缩放的屏上偏掉。
#[tauri::command]
pub fn viewer_pin_move_preset(
    app: tauri::AppHandle,
    label: String,
    preset: String,
) -> Result<(), String> {
    let Some(w) = app.get_webview_window(&label) else {
        return Err("浮窗已关闭".into());
    };
    let monitor = w
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return Err("找不到显示器".into());
    };
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let size = w
        .outer_size()
        .unwrap_or(PhysicalSize::new(DEFAULT_W, DEFAULT_H));
    // 工作区换算成逻辑像素（Tauri 的 set_position 收逻辑坐标，它会按目标
    // 显示器的缩放再换回物理像素 —— 直接给物理像素在 150% 屏上会偏）。
    let wx = work.position.x as f64 / scale;
    let wy = work.position.y as f64 / scale;
    let ww = work.size.width as f64 / scale;
    let wh = work.size.height as f64 / scale;
    let cw = size.width as f64 / scale;
    let ch = size.height as f64 / scale;
    let (x, y) = match preset.as_str() {
        "left-top" => (wx, wy),
        "right-top" => (wx + ww - cw, wy),
        "left-bottom" => (wx, wy + wh - ch),
        "right-bottom" => (wx + ww - cw, wy + wh - ch),
        "center-left" => (wx, wy + (wh - ch) / 2.0),
        "center-right" => (wx + ww - cw, wy + (wh - ch) / 2.0),
        "center-top" => (wx + (ww - cw) / 2.0, wy),
        _ => (wx + (ww - cw) / 2.0, wy + (wh - ch) / 2.0),
    };
    w.set_position(LogicalPosition::new(x, y))
        .map_err(|e| format!("移动浮窗失败: {e}"))?;
    Ok(())
}

/// 预设位置清单（菜单用）。
#[tauri::command]
pub fn viewer_pin_presets() -> Vec<PinPreset> {
    vec![
        PinPreset { key: "left-top", label: "左上" },
        PinPreset { key: "center-top", label: "上中" },
        PinPreset { key: "right-top", label: "右上" },
        PinPreset { key: "center-left", label: "左中" },
        PinPreset { key: "center", label: "居中" },
        PinPreset { key: "center-right", label: "右中" },
        PinPreset { key: "left-bottom", label: "左下" },
        PinPreset { key: "center-bottom", label: "下中" },
        PinPreset { key: "right-bottom", label: "右下" },
    ]
}

/// 主窗口关闭时连带关掉所有浮窗（浮窗是主流程的"例外"，不该留成孤儿）。
pub fn close_all_pins(app: &tauri::AppHandle) {
    for (label, _) in pin_windows(app) {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encoding_round_trips() {
        let raw = r"<USERPROFILE>\我的 文件\a#b?c.docx";
        let enc = encode_query(raw);
        assert!(!enc.contains(' '));
        assert!(!enc.contains('#'));
        assert_eq!(percent_decode(&enc), raw);
    }

    #[test]
    fn preset_list_covers_eight_positions() {
        let p = viewer_pin_presets();
        assert_eq!(p.len(), 9);
        assert!(p.iter().any(|x| x.key == "center"));
        assert!(p.iter().any(|x| x.key == "right-bottom"));
    }
}
