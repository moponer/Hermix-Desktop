// 截图会话控制器：覆盖窗生命周期 + Tauri 命令。
// 数据通路见 screenshot.rs（抓屏/裁剪/剪贴板）与
// docs/notes/implemented/feature/2026-10-07-screenshot-tool.md。
//
// 2026-10-10 结构修复（"启动慢 + 全屏白启动屏"回归）：
// ① 窗口在抓屏后（~200ms）即创建，幕布 JPEG 编码移到后台线程——与页面
//    加载并行；前端轮询 screenshot_frame_data 到 Ready 才 show 窗口。
//    感知等待从「编码+加载+IPC 串行」降为 max(编码, 加载)，且页面
//    首帧即有「正在截取屏幕…」反馈（index.html #overlay-boot 静态占位）。
// ② 旧的 3s 兜底 show() 已废除：页面没加载完就亮窗会把 #splash（静态
//    HTML，启动屏样式）糊成全屏——9945d406 只在 JS 层删 splash，兜底
//    路径没堵住，这是"修好了又回去"的根因。现改 30s 兜底「关窗+报错」：
//    窗口要么在 Ready 后被前端 show（正常），要么 30s 关闭，绝不留白屏。
// ③ 幕布走 JPEG（photo of screen，体积/编码/传输全小）；编辑底按需
//    走 screenshot_crop_data（选区无损 PNG，比整帧 PNG 小且更清晰）。

use crate::screenshot;
use base64::Engine as _;
use serde::Serialize;
use tauri::{Emitter, Manager};

const OVERLAY_LABEL: &str = "screenshot-overlay";

/// 覆盖窗的 WebView2 环境参数。必须与 tauri.conf.json 主窗口的
/// additionalBrowserArgs **逐字一致**：同一 user data folder 下所有
/// webview 共享一个浏览器进程，环境级选项不一致时创建直接报
/// 0x8007139F（ERROR_INVALID_STATE）——覆盖窗此前没带这份参数，
/// 就是「覆盖窗创建失败」的根因。改主窗参数时这里必须同步。
const OVERLAY_BROWSER_ARGS: &str = "--disable-features=msSmartScreenProtection,EdgeFeed,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling,RendererCodeIntegrity --disable-renderer-backgrounding --disable-background-timer-throttling --disk-cache-size=20971520 --js-flags=--max-old-space-size=1024";
/// setup 默认注册的热键，与 lib.rs 保持一致；前端回放配置前的兜底。
const DEFAULT_HOTKEY: &str = "alt+Q";
/// 会话总兜底：编码 + 页面加载正常以秒计，30s 没就绪即判异常收尾。
const SESSION_FALLBACK_SECS: u64 = 30;

/// 截图功能开关（设置 → 图像）。默认开；false 时热键与标题栏入口都拒绝。
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// 当前已注册的全局热键字符串（apply 时先注册新的再解旧的，失败不破坏现状）。
static REGISTERED_HOTKEY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Serialize, Clone)]
pub struct FramePayload {
    /// 全屏照片的 JPEG data URL，覆盖窗直接铺 <img>
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

/// 幕布帧状态机：None = 编码中（前端继续轮询）；Ready = 渲染并 show；
/// Failed = 前端显示错误 UI（ESC / Rust 兜底监视仍可退出）。
/// untagged 序列化：Ready 平铺为 FramePayload，Failed 为 {"error": …}。
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum FrameState {
    Ready(FramePayload),
    Failed { error: String },
}

static FRAME: once_cell::sync::OnceCell<std::sync::Mutex<Option<FrameState>>> =
    once_cell::sync::OnceCell::new();

fn frame() -> &'static std::sync::Mutex<Option<FrameState>> {
    FRAME.get_or_init(|| std::sync::Mutex::new(None))
}

/// 入口命令：异步包装——抓屏 + 建窗放阻塞线程池，主线程零阻塞。
#[tauri::command]
pub async fn start_screenshot(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || run_session(&app))
        .await
        .map_err(|e| format!("截图任务调度失败: {e}"))?
}

/// 会话主流程（快：抓屏 ~200ms + 建窗 + 起三个后台线程即返回）。
/// 感知「出画面」= 抓屏 + max(后台 JPEG 编码, 页面加载)，不再是串行和。
pub fn run_session(app: &tauri::AppHandle) -> Result<(), String> {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("截图功能已在设置 → 图像中关闭".into());
    }
    // 重入：先关旧覆盖窗（正常路径不会出现，热键连按兜底）。
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }

    let session = screenshot::capture_monitor_at_cursor()?;
    // 窗口几何（逻辑坐标）在 move 进缓存前先拷出。
    let (pos_x, pos_y, vw, vh, scale) = (
        session.origin_x as f64,
        session.origin_y as f64,
        session.width as f64,
        session.height as f64,
        session.scale,
    );
    *screenshot::session().lock().unwrap() = Some(session);
    *frame().lock().unwrap() = None; // 编码中；前端轮询

    // 全屏覆盖窗：无边框、置顶、跳任务栏。仍先建后亮（hidden）——前端
    // 拉到 Ready 才 show，"白屏/启动屏"不可能被看到；页面没就绪时窗口
    // 保持隐藏，由 30s 兜底收尾，而不是 3s 亮出半就绪页面。
    // M1 范围：单显示器（光标所在屏），builder.position 为逻辑坐标，
    // 主屏原点 (0,0) 下与虚拟屏坐标一致；多屏混排 DPI 留 M3。

    let built = tauri::WebviewWindowBuilder::new(
        app,
        OVERLAY_LABEL,
        tauri::WebviewUrl::App("index.html#/screenshot-overlay".into()),
    )
    .title("Screenshot")
    .additional_browser_args(OVERLAY_BROWSER_ARGS)
    // 主窗是 InPrivate（conf incognito: true），覆盖窗对齐——截图内容不该
    // 落进任何持久化 profile。这是 controller 级选项，不影响环境共享。
    .incognito(true)
    .position(pos_x, pos_y)
    .inner_size(vw / scale, vh / scale)
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

    spawn_encoder(app.clone());
    spawn_esc_watcher(app.clone());
    spawn_session_fallback(app.clone());

    Ok(())
}

/// 后台编码线程：BGRA→RGBA → JPEG(q80) → base64 → FrameState。
/// 先 take 出 session 独占（像素量级 10-30MB，不 clone）；完成后归还，
/// 供编辑器按需裁剪（screenshot_crop_data）。若期间窗口已关（用户 ESC /
/// 确认走了 cancel），直接清空不留残。
fn spawn_encoder(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let Some(session) = screenshot::session().lock().unwrap().take() else {
            *frame().lock().unwrap() =
                Some(FrameState::Failed { error: "截图会话不存在".into() });
            return;
        };
        let payload = (|| {
            let rgba = screenshot::bgra_to_rgba(&session.pixels);
            let jpeg = screenshot::encode_jpeg(&rgba, session.width, session.height)?;
            Ok(FramePayload {
                data_url: format!(
                    "data:image/jpeg;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(jpeg)
                ),
                origin_x: session.origin_x,
                origin_y: session.origin_y,
                width: session.width,
                height: session.height,
                scale: session.scale,
            })
        })();

        // 归还 session（或会话已结束则清空）。
        let alive = app.get_webview_window(OVERLAY_LABEL).is_some();
        *screenshot::session().lock().unwrap() = alive.then_some(session);

        *frame().lock().unwrap() = match payload {
            Ok(p) if alive => Some(FrameState::Ready(p)),
            Ok(_) => None,
            Err(e) => Some(FrameState::Failed { error: e }),
        };
    });
}

/// 会话兜底（替代旧 3s 兜底 show）：30s 后窗口仍存在且前端没亮（is_visible
/// = false）即判页面/编码异常——报错回主窗 toast 并关窗。前端已 show 的
/// 会话（用户在选区/编辑中）不动。
fn spawn_session_fallback(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(SESSION_FALLBACK_SECS));
        let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
            return; // 会话已正常结束
        };
        if win.is_visible().unwrap_or(false) {
            return; // 前端已就绪，用户在正常交互
        }
        let failed = !matches!(frame().lock().unwrap().as_ref(), Some(FrameState::Ready(_)));
        if failed {
            let _ = app.emit(
                "hermix:screenshot-error",
                "截图覆盖层 30s 未就绪，已关闭。若反复出现请重启应用。",
            );
        }
        let _ = win.close();
    });
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

/// 覆盖窗轮询幕布帧。返回 None = 仍在编码（继续轮询）；Ready = 渲染 +
/// show；Failed = 错误 UI（前端有独立超时兜底）。
#[tauri::command]
pub fn screenshot_frame_data() -> Option<FrameState> {
    frame().lock().unwrap().clone()
}

/// 编辑器按需取选区：帧图局部物理像素 → 裁出无损 PNG data URL。
/// 幕布是 JPEG（体积小、供选区交互），编辑底走本命令从 RAW 会话位图
/// 无损裁剪——传输更小（只传选区不传整帧）、输出更清晰（不走幕布质量）。
#[tauri::command]
pub fn screenshot_crop_data(x: i32, y: i32, width: i32, height: i32) -> Result<String, String> {
    use crate::screenshot::Selection;
    // guard 必须绑定到局部变量活过整个函数体（锁临时值句尾释放会让 s 悬空）。
    let guard = screenshot::session().lock().unwrap();
    let s = guard
        .as_ref()
        .ok_or("截图会话已结束（幕布仍在编码或已被取消）")?;

    // 入参为帧图局部物理像素；先钳出真实裁切尺寸（越界部分截去），
    // 再换回虚拟屏坐标喂给 crop_rgba（它按 session 原点平移 + 再钳位）。
    let sx = x.clamp(0, s.width as i32);
    let sy = y.clamp(0, s.height as i32);
    let ex = (x + width).clamp(sx, s.width as i32);
    let ey = (y + height).clamp(sy, s.height as i32);
    let (cw, ch) = (ex - sx, ey - sy);
    if cw <= 0 || ch <= 0 {
        return Err("选区在帧图之外，无法裁剪".into());
    }

    let rgba = screenshot::crop_rgba(
        s,
        Selection {
            x: s.origin_x + sx,
            y: s.origin_y + sy,
            width: cw,
            height: ch,
        },
    )
    .ok_or("选区在帧图之外，无法裁剪")?;
    let png = screenshot::encode_png(&rgba, cw as u32, ch as u32)?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
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

/// 编辑器提交：前端把编辑后的画布导出为 PNG data URL 回传，
/// 解码 → 剪贴板 → 关覆盖窗。选区裁剪已在前端画布完成，Rust 只收成品。
#[tauri::command]
pub fn screenshot_commit_edited(app: tauri::AppHandle, data_url: String) -> Result<(), String> {
    const PREFIX: &str = "data:image/png;base64,";
    let b64 = data_url
        .strip_prefix(PREFIX)
        .ok_or_else(|| "data URL 前缀无效".to_string())?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("base64 解码失败: {e}"))?;
    let img = image::load_from_memory(&png)
        .map_err(|e| format!("PNG 解码失败: {e}"))?
        .to_rgba8();
    let (width, height) = img.dimensions();

    match screenshot::copy_png_to_clipboard(img.as_raw(), width, height) {
        Ok(()) => {
            *screenshot::session().lock().unwrap() = None;
            *frame().lock().unwrap() = None;
            close_overlay(&app);
            let _ = app.emit("hermix:screenshot-copied", ());
            Ok(())
        }
        Err(e) => {
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

/// 应用截图配置（设置 → 图像）：开关 + 全局热键。
/// 顺序是"先注册新的、成功后再解旧的"——注册失败（被占用）时旧热键
/// 仍在，现状不被破坏。无存储配置时前端不调用，setup 默认热键继续生效。
#[tauri::command]
pub fn screenshot_apply_config(
    app: tauri::AppHandle,
    enabled: bool,
    hotkey: String,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    if !enabled {
        // 关闭：解掉当前热键（含 setup 默认注册的兜底）。
        let mut registered = REGISTERED_HOTKEY.lock().unwrap();
        let prev = registered.take().unwrap_or_else(|| DEFAULT_HOTKEY.into());
        if let Ok(sc) = prev.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(sc);
        }
        return Ok(());
    }

    let gs = app.global_shortcut();
    let sc: Shortcut = hotkey
        .parse()
        .map_err(|e| format!("热键 {hotkey} 格式无效: {e}"))?;
    gs.register(sc)
        .map_err(|e| format!("热键 {hotkey} 注册失败（可能被其他程序占用）: {e}"))?;

    let mut registered = REGISTERED_HOTKEY.lock().unwrap();
    let prev = registered
        .replace(hotkey.clone())
        .unwrap_or_else(|| DEFAULT_HOTKEY.into());
    if prev != hotkey {
        if let Ok(old) = prev.parse::<Shortcut>() {
            let _ = gs.unregister(old);
        }
    }
    Ok(())
}

fn close_overlay(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.close();
    }
}
