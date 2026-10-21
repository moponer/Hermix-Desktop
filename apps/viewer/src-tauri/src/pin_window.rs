//! 钉住浮窗：把某个文件 / 目录的预览**钉在屏幕某个位置**。
//!
//! 用户诉求：「某个文件夹下我想将该目录钉在屏幕某个位置，方便多个窗口不用
//! 来回切；某个文件的预览也可以钉在某个位置，方便对比」。
//!
//! ## 关键实现选择：浮窗加载**主窗口自己的 URL**
//!
//! 浮窗的 webview 用 `WebviewUrl::External(主窗口 URL)`（**不带任何
//! query**），**不是** `WebviewUrl::App(...)`。原因（2026-10-21 实测）：
//! - `WebviewUrl::App("index.html?pin=…")` → Tauri 把 `?pin=…` 当成文件名
//!   的一部分去找前端资源 → **建窗直接失败**；
//! - 地址**带 query**（`http://tauri.localhost/?pin=…`）→ 资源服务按
//!   「带 query 的路径」找不到前端资源 → **整窗空白**；
//! - 地址**不带 query** → 与在子 webview 里实测过的地址完全一致 → 正常
//!   渲染，IPC 也通。
//!
//! 浮窗的身份（被钉住的是谁）**不进 URL**：前端按自己的窗口 label 去 Rust
//! 侧查（`pin_path`）。label 是运行期就有的权威身份，不进 URL，自然没有
//! 「资源服务不认 query」的问题。
//!
//! ## 坐标语义
//!
//! Rust 侧 `PhysicalPosition` / `PhysicalSize` 是物理像素，显示器坐标也是
//! 物理像素；但**预设位置**用逻辑像素交给 Tauri，它按目标显示器的缩放换算
//! （直接给物理像素在 150% 缩放的屏上会偏）。
//!
//! ## 落位记忆要记「显示器是谁」
//!
//! 便携应用拔着 U 盘换机器是常态：显示器拔了以后按旧坐标恢复 = 窗口跑到
//! 屏幕外。落位记忆里存 `Monitor::name()` + `scale_factor`，恢复时原屏不在
//! （或 DPI 变了）就落到主屏的 **work_area**（工作区，不含任务栏 —— 用
//! `size` 会被任务栏压住）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

/// 浮窗 label 前缀（capability 用 `pin-*` 通配放行）。
const PIN_PREFIX: &str = "pin-";
/// 同时最多几个浮窗（每个浮窗 = 一个独立 WebView2 实例，内存线性增长）。
const MAX_PINS: usize = 6;
/// 浮窗默认尺寸（逻辑像素）。
const DEFAULT_SIZE: (f64, f64) = (760.0, 600.0);
/// 贴边吸附阈值（逻辑像素）：拖到离边这么近就吸上去。
const SNAP_EDGE: f64 = 28.0;
/// 自动排版时工作区四周留的边距（逻辑像素）。
const LAYOUT_MARGIN: f64 = 16.0;
/// 自动排版时格子之间的缝（逻辑像素）。
const LAYOUT_GAP: f64 = 12.0;

/// 一个钉住的预览。
#[derive(Clone, Debug)]
struct PinEntry {
    label: String,
    path: String,
}

/// 已钉住预览的对外信息。
#[derive(Serialize, Clone, Debug)]
pub struct PinInfo {
    pub label: String,
    pub path: String,
    pub title: String,
}

/// 落位记忆（一条 = 一个被钉住的路径）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct PinRecord {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// 当时的显示器名（`Monitor::name()`）。
    monitor: Option<String>,
    /// 当时的缩放比。
    scale: f64,
}

/// 落位记忆文件：`$hermix_home/pinned-windows.json`。
fn record_path() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join("pinned-windows.json")
}

/// 读落位记忆（坏了就当没有）。
fn load_records() -> HashMap<String, PinRecord> {
    std::fs::read_to_string(record_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// 写落位记忆（失败不阻断使用——位置记不住只是下次要重新摆）。
fn save_records(records: &HashMap<String, PinRecord>) {
    let path = record_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string_pretty(records) {
        let _ = std::fs::write(&path, body);
    }
}

/// 当前钉住的预览（label → 条目）。
fn pins() -> &'static Mutex<HashMap<String, PinEntry>> {
    static P: OnceLock<Mutex<HashMap<String, PinEntry>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// label 计数器（pin-1、pin-2 …）。
fn seq() -> &'static AtomicU64 {
    static S: OnceLock<AtomicU64> = OnceLock::new();
    S.get_or_init(|| AtomicU64::new(0))
}

/// 文件名（浮窗标题用）。
fn display_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 钉住一个路径（文件或目录）：已钉过就聚焦，没钉过就开新窗。
///
/// **必须是 `async` 命令**：Tauri 的同步命令跑在**主线程**上，而 Windows 上
/// 创建 webview 必须离开主线程（wry#583）——主线程被占住时，webview 建得出来
/// 但**永远停在 `about:blank`**（导航事件排队却没人处理）。这条是本功能第一
/// 版"浮窗空白"的真根因（2026-10-21 实测：CDP 里该 target 的 url 是
/// `about:blank`，DOM 长度 0）。
#[tauri::command]
pub async fn pin_open(app: AppHandle, path: String) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("没有可钉住的路径".into());
    }
    // 同一路径重复钉 = 复用既有浮窗（不刷出一堆重复窗口）。
    {
        let map = pins().lock().unwrap();
        if let Some(existing) = map.values().find(|e| e.path == path) {
            let label = existing.label.clone();
            drop(map);
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            return Ok(label);
        }
    }

    // 超上限时关掉最早的那个。
    {
        let mut map = pins().lock().unwrap();
        while map.len() >= MAX_PINS {
            let Some(oldest) = map.keys().next().cloned() else {
                break;
            };
            map.remove(&oldest);
            if let Some(window) = app.get_webview_window(&oldest) {
                let _ = window.close();
            }
        }
    }

    let label = format!("{PIN_PREFIX}{}", seq().fetch_add(1, Ordering::Relaxed) + 1);

    // 落位：有记忆就用记忆的（显示器还在且 DPI 没变才用），否则自动排版。
    let mut records = load_records();
    let record = records.get(&path).cloned();
    let pinned_count = pins().lock().unwrap().len();
    let (x, y, w, h) = match record {
        Some(rec) if monitor_still_valid(&app, &rec) => (rec.x, rec.y, rec.w, rec.h),
        _ => tiled_position(&app, pinned_count, pinned_count + 1),
    };

    // 浮窗加载**应用自己的资源**，走 `WebviewUrl::App`（与主窗口同一条路）。
    //
    // 实测（2026-10-21，CDP 取证）：
    // - `WebviewUrl::External("http://tauri.localhost/")` 在**顶层窗口**里
    //   拿不到应用的协议处理器 → 页面是 "ERR_CONNECTION_REFUSED"（真空白，
    //   不是截图假象）；
    // - 同样的地址在**子 webview**里却能正常渲染（子 webview 走的是另一条
    //   注入路径）。
    // 所以顶层浮窗用 `WebviewUrl::App` —— 主窗口就是它这么起来的。
    //
    // 浮窗的身份（被钉住的是谁）**不进 URL**：前端按自己的窗口 label 去 Rust
    // 侧查（`pin_path`）。
    //
    // **不设 always_on_top**：置顶浮窗会把别的程序压在下面 —— 用户手动激活
    // 另一个程序时它应该能盖上来（"连查看器自己都被遮住了"）。浮窗靠
    // "后开的窗口在上"这一默认 z 序与主窗口共存即可。
    let window = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .title(format!("{} · Hermix", display_name(&path)))
        .inner_size(w, h)
        .position(x, y)
        .resizable(true)
        .build()
        .map_err(|e| format!("浮窗创建失败：{e}"))?;
    let _ = window.set_focus();

    // 移动 / 缩放后立刻记住落位（拖动过程中会连着来很多事件 → 写文件很
    // 频繁，所以这里只更新内存里的记录，落盘由 `save_records` 在松手后
    // 做：见下面的节流）。
    let app_handle = app.clone();
    let tracked = window.clone();
    let tracked_label = label.clone();
    window.on_window_event(move |event| {
        match event {
            WindowEvent::Moved(pos) => {
                // 贴边吸附：拖到屏幕边缘附近就吸上去（Windows 11 的贴靠、
                // macOS 的窗口贴边都是同一套交互）。
                snap_to_edge(&tracked, *pos);
            }
            WindowEvent::Resized(_) => {}
            _ => return,
        }
        let (Ok(pos), Ok(size)) = (tracked.outer_position(), tracked.outer_size()) else {
            return;
        };
        let scale = tracked.scale_factor().unwrap_or(1.0);
        remember_position(
            &app_handle,
            &tracked_label,
            pos.x as f64 / scale,
            pos.y as f64 / scale,
            size.width as f64 / scale,
            size.height as f64 / scale,
            false,
        );
    });

    pins().lock().unwrap().insert(
        label.clone(),
        PinEntry {
            label: label.clone(),
            path: path.clone(),
        },
    );
    // 首次落位就写进记忆（用户关窗也不丢位置）。
    records.insert(
        path,
        PinRecord {
            x,
            y,
            w,
            h,
            monitor: current_monitor_name(&app),
            scale: current_scale(&app),
        },
    );
    save_records(&records);
    let _ = window;
    Ok(label)
}

/// 关掉一个浮窗。
#[tauri::command]
pub fn pin_close(app: AppHandle, label: String) -> Result<(), String> {
    // 关窗前把最后的落位**强制**写进记忆（拖动时的事件是节流的，最后
    // 一下可能被节流掉了）。
    if let Some(window) = app.get_webview_window(&label) {
        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
            let scale = window.scale_factor().unwrap_or(1.0);
            remember_position(
                &app,
                &label,
                pos.x as f64 / scale,
                pos.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
                true,
            );
        }
        let _ = window.close();
    }
    pins().lock().unwrap().remove(&label);
    Ok(())
}

/// 当前钉住的预览列表。
#[tauri::command]
pub fn pin_list() -> Vec<PinInfo> {
    let map = pins().lock().unwrap();
    map.values()
        .map(|e| PinInfo {
            label: e.label.clone(),
            path: e.path.clone(),
            title: display_name(&e.path),
        })
        .collect()
}

/// 浮窗问自己的路径（前端入口按 label 查）。
#[tauri::command]
pub fn pin_path(label: String) -> Option<String> {
    pins()
        .lock()
        .unwrap()
        .get(&label)
        .map(|e| e.path.clone())
}

/// 浮窗移动 / 缩放后更新落位记忆。
///
/// `force` = 一定要落盘（关窗时）。拖动过程中是节流的：一次拖动能连着来
/// 上百个 Moved/Resized 事件，每个都写一次 JSON 是纯浪费。
pub fn remember_position(app: &AppHandle, label: &str, x: f64, y: f64, w: f64, h: f64, force: bool) {
    if !label.starts_with(PIN_PREFIX) {
        return;
    }
    let Some(path) = pins().lock().unwrap().get(label).map(|e| e.path.clone()) else {
        return;
    };
    let mut records = load_records();
    records.insert(
        path,
        PinRecord {
            x,
            y,
            w,
            h,
            monitor: current_monitor_name(app),
            scale: current_scale(app),
        },
    );
    if force || write_throttle_passed() {
        save_records(&records);
    }
}

/// 落盘节流：两次落盘至少隔 600ms（拖动时事件很密）。
fn write_throttle_passed() -> bool {
    static LAST: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    let last = LAST.get_or_init(|| std::sync::atomic::AtomicU64::new(0));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = last.load(Ordering::Relaxed);
    if now.saturating_sub(prev) > 600 {
        last.store(now, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// 显示器是否还在（名字对得上 + 缩放没变）。
fn monitor_still_valid(app: &AppHandle, record: &PinRecord) -> bool {
    let Some(name) = record.monitor.as_ref() else {
        return false;
    };
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };
    monitors.iter().any(|m| {
        m.name().map(|n| n == name).unwrap_or(false)
            && (record.scale - m.scale_factor()).abs() < 0.01
    })
}

/// 当前显示器名。
fn current_monitor_name(app: &AppHandle) -> Option<String> {
    let window = app.get_webview_window("main")?;
    let monitor = window.current_monitor().ok().flatten()?;
    monitor.name().cloned()
}

/// 当前缩放比。
fn current_scale(app: &AppHandle) -> f64 {
    app.get_webview_window("main")
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0)
}

/// 默认摆位：多个浮窗按网格自动排版，互不重叠。
///
/// 上一版是"每开一个就斜着错开 28px"—— 三个窗口叠在一起，谁也看不见谁，
/// 等于没有排版。桌面端成熟的做法（Windows 贴靠分屏、macOS Stage Manager、
/// PowerToys FancyZones）是**把工作区切成格子，每个窗口占一格**：这里取
/// 最朴素的一种：按 √n 切成近似正方的网格，按开启顺序依次填格。
fn tiled_position(app: &AppHandle, index: usize, total: usize) -> (f64, f64, f64, f64) {
    let Some(monitor) = app.primary_monitor().ok().flatten() else {
        return (80.0, 80.0, DEFAULT_SIZE.0, DEFAULT_SIZE.1);
    };
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let ox = area.position.x as f64 / scale;
    let oy = area.position.y as f64 / scale;
    let work_w = area.size.width as f64 / scale;
    let work_h = area.size.height as f64 / scale;

    let total = total.max(1);
    let cols = (total as f64).sqrt().ceil().max(1.0) as usize;
    let rows = total.div_ceil(cols).max(1);
    let cell_w = (work_w - LAYOUT_MARGIN * 2.0 - LAYOUT_GAP * (cols as f64 - 1.0)) / cols as f64;
    let cell_h = (work_h - LAYOUT_MARGIN * 2.0 - LAYOUT_GAP * (rows as f64 - 1.0)) / rows as f64;

    // 窗口不超过格子（也不小于一个可用的下限）。
    let w = cell_w.clamp(320.0, DEFAULT_SIZE.0);
    let h = cell_h.clamp(240.0, DEFAULT_SIZE.1);
    let col = index % cols;
    let row = index / cols;
    let x = ox + LAYOUT_MARGIN + col as f64 * (cell_w + LAYOUT_GAP);
    let y = oy + LAYOUT_MARGIN + row as f64 * (cell_h + LAYOUT_GAP);
    (x, y, w, h)
}

/// 贴边吸附：窗口离屏幕工作区某条边在阈值内就把它推上去。
///
/// 拖动过程中 Windows 会用自己的位置覆盖我们设的值，但**拖动结束后的最后
/// 一次 Moved 事件**会再走一遍这里 —— 那时吸附就生效了。已吸到边上的
/// 窗口再次收到事件时算出的目标位置与当前位置相同，不会再触发移动，
/// 因此不会自激。
fn snap_to_edge(window: &WebviewWindow, pos: PhysicalPosition<i32>) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let area = monitor.work_area();
    let ax = area.position.x as f64 / scale;
    let ay = area.position.y as f64 / scale;
    let aw = area.size.width as f64 / scale;
    let ah = area.size.height as f64 / scale;
    let Ok(size) = window.outer_size() else {
        return;
    };
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let lx = pos.x as f64 / scale;
    let ly = pos.y as f64 / scale;

    let mut nx = lx;
    let mut ny = ly;
    if (lx - ax).abs() <= SNAP_EDGE {
        nx = ax;
    } else if ((lx + w) - (ax + aw)).abs() <= SNAP_EDGE {
        nx = ax + aw - w;
    }
    if (ly - ay).abs() <= SNAP_EDGE {
        ny = ay;
    } else if ((ly + h) - (ay + ah)).abs() <= SNAP_EDGE {
        ny = ay + ah - h;
    }
    if nx == lx && ny == ly {
        return;
    }
    let target = PhysicalPosition::new((nx * scale) as i32, (ny * scale) as i32);
    let _ = window.set_position(target);
}

/// 把浮窗摆到工作区的某个方位（工具条上的「贴左 / 贴右 / 居中」）。
#[tauri::command]
pub fn pin_place(app: AppHandle, label: String, preset: String) -> Result<(), String> {
    let Some(window) = app.get_webview_window(&label) else {
        return Err("浮窗不存在".into());
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        return Err("找不到当前显示器".into());
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let area = monitor.work_area();
    let ax = area.position.x as f64 / scale;
    let ay = area.position.y as f64 / scale;
    let aw = area.size.width as f64 / scale;
    let ah = area.size.height as f64 / scale;
    let Ok(size) = window.outer_size() else {
        return Err("读不到浮窗尺寸".into());
    };
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let Ok(pos) = window.outer_position() else {
        return Err("读不到浮窗位置".into());
    };
    let lx = pos.x as f64 / scale;
    let ly = pos.y as f64 / scale;

    let (nx, ny) = match preset.as_str() {
        "left" => (ax, (ly - ay).clamp(0.0, (ah - h).max(0.0))),
        "right" => (ax + aw - w, (ly - ay).clamp(0.0, (ah - h).max(0.0))),
        "top" => ((lx - ax).clamp(0.0, (aw - w).max(0.0)), ay),
        "bottom" => ((lx - ax).clamp(0.0, (aw - w).max(0.0)), ay + ah - h),
        "center" => (ax + (aw - w) / 2.0, ay + (ah - h) / 2.0),
        _ => return Err(format!("未知的摆位：{preset}")),
    };
    let target = PhysicalPosition::new((nx * scale) as i32, (ny * scale) as i32);
    window
        .set_position(target)
        .map_err(|e| format!("移动浮窗失败：{e}"))
}

/// 把逻辑坐标换算回物理像素（落位记忆用物理像素存更稳）。
pub fn to_physical(x: f64, y: f64, w: f64, h: f64, scale: f64) -> (f64, f64, f64, f64) {
    (x * scale, y * scale, w * scale, h * scale)
}

/// 供 lib.rs 使用的逻辑尺寸助手（避免调用方重复算）。
pub fn default_size() -> LogicalSize<f64> {
    LogicalSize::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
}

/// 供 lib.rs 使用的逻辑位置助手。
pub fn logical_position(x: f64, y: f64) -> LogicalPosition<f64> {
    LogicalPosition::new(x, y)
}
