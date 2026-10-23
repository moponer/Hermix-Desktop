// 截图会话控制器：覆盖窗生命周期 + Tauri 命令。
// 数据通路见 screenshot.rs（抓屏/裁剪/剪贴板）与
// docs/notes/implemented/feature/2026-10-23-screenshot-rewrite.md。
//
// 2026-10-23 结构重写（"打不开 / 慢 / 黑底 / 选多大不显示多大"四件套根修）：
// ① **覆盖窗只创建一次、反复复用**。原来每次截图都新建一个 WebView2
//    窗口（含页面加载），第二次起每次都白扔一次建窗 + 加载开销。现在
//    首开建窗，之后截图只是「定位 + 显示 + 推帧」。
// ② **幕布帧改事件推送**（hermix:screenshot-frame），删掉原来 150ms
//    一次的轮询——轮询粒度本身就是感知的等待下限。
// ③ **亮窗时机交给页面**：页面把幕布解码完再回报 screenshot_overlay_ready，
//    Rust 才 show。窗口要么显示已就绪的画面，要么根本不出现，没有黑屏。
// ④ **选区即窗口**：选完区 Rust 把覆盖窗缩到选区大小，编辑器只显示
//    选中的那部分（原设计是全屏黑底 + 中间一小块图）。
// ⑤ **WebView2 参数单一来源**：覆盖窗不再单独传 additional_browser_args。
//    同一 user data folder 下所有 webview 的浏览器参数必须一致，不一致
//    建窗直接 0x8007139F（ERROR_INVALID_STATE）——此前主窗走 conf 的
//    参数、覆盖窗走另一份、进程环境变量还有第三份，三份不一致就是
//    "截图打不开"。现在统一由 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS 提供。

use crate::screenshot;
use base64::Engine as _;
use serde::Serialize;
use tauri::{Emitter, Manager};

const OVERLAY_LABEL: &str = "screenshot-overlay";
/// 幕布帧事件名（页面订阅它拿新帧）。
const FRAME_EVENT: &str = "hermix:screenshot-frame";
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
    /// 位图原点（虚拟屏幕坐标，物理像素）——覆盖窗把本地坐标换算回去用
    origin_x: i32,
    origin_y: i32,
    /// 物理像素尺寸（= 虚拟桌面尺寸）
    width: u32,
    height: u32,
    /// 会话序号：页面据此丢弃上一轮残留的迟到事件
    seq: u64,
}

/// 幕布帧状态机：None = 编码中（页面等事件）；Ready = 渲染并 show；
/// Failed = 页面显示错误 UI（ESC / Rust 兜底监视仍可退出）。
/// untagged 序列化：Ready 平铺为 FramePayload，Failed 为 {"error": …}。
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum FrameState {
    Ready(FramePayload),
    Failed { error: String },
}

/// 覆盖窗会话状态（全局单例）。
struct OverlayState {
    /// 当前幕布帧（None = 编码中）
    frame: Option<FrameState>,
    /// 是否有一个进行中的截图会话（ESC 监视 / 兜底收尾据此判断）
    active: bool,
    /// 会话序号（每开一轮 +1）
    seq: u64,
    /// 截图前的前台窗口（截完把焦点还回去）
    fg_hwnd: Option<isize>,
}

static STATE: once_cell::sync::OnceCell<std::sync::Mutex<OverlayState>> =
    once_cell::sync::OnceCell::new();

fn state() -> &'static std::sync::Mutex<OverlayState> {
    STATE.get_or_init(|| {
        std::sync::Mutex::new(OverlayState {
            frame: None,
            active: false,
            seq: 0,
            fg_hwnd: None,
        })
    })
}

/// 入口命令：异步包装——抓屏 + 建窗放阻塞线程池，主线程零阻塞。
#[tauri::command]
pub async fn start_screenshot(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || run_session(&app))
        .await
        .map_err(|e| format!("截图任务调度失败: {e}"))?
}

/// 会话主流程（快：抓屏 ~10ms + 定位窗口 + 推帧）。
/// 感知「出画面」= 抓屏 + 后台 JPEG 编码 + 页面解码，不再有建窗与页面加载。
pub fn run_session(app: &tauri::AppHandle) -> Result<(), String> {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("截图功能已在设置 → 图像中关闭".into());
    }
    // 重入：结束上一轮（覆盖窗复用，不销毁）。
    end_session(app, false);

    // 一次抓整块虚拟桌面：多显示器一次覆盖，屏间无黑缝。
    let session = screenshot::capture_desktop()?;
    let (ox, oy, w, h) = (
        session.origin_x,
        session.origin_y,
        session.width,
        session.height,
    );
    let fg = screenshot::foreground_window();
    *screenshot::session().lock().unwrap() = Some(session);

    // 覆盖窗：首次创建，之后复用（省掉 WebView2 建窗 + 页面加载）。
    let win = ensure_overlay(app)?;
    place_window(&win, ox, oy, w, h);

    {
        let mut st = state().lock().unwrap();
        st.frame = None; // 编码中
        st.active = true;
        st.seq += 1;
        st.fg_hwnd = fg;
    }
    let seq = state().lock().unwrap().seq;

    spawn_encoder(app.clone(), seq);
    spawn_esc_watcher(app.clone());
    spawn_session_fallback(app.clone());

    Ok(())
}

/// 取/建覆盖窗。已存在就复用——这是"第二次起快一个数量级"的关键。
fn ensure_overlay(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        return Ok(win);
    }
    let built = tauri::WebviewWindowBuilder::new(
        app,
        OVERLAY_LABEL,
        tauri::WebviewUrl::App("index.html#/screenshot-overlay".into()),
    )
    .title("Screenshot")
    // 主窗是 InPrivate（conf incognito: true），覆盖窗对齐——截图内容不该
    // 落进任何持久化 profile。这是 controller 级选项，不影响环境共享。
    .incognito(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false)
    // 隐藏状态下不抢焦点；真正显示时再 set_focus。
    .focused(false)
    .build();

    built.map_err(|e| {
        // 覆盖窗建不出来最常见的原因是 WebView2 浏览器参数与主窗不一致
        // （0x8007139F）。参数现在统一来自环境变量，这里补一句可读提示。
        format!("覆盖窗创建失败: {e}（若为 0x8007139F，说明 WebView2 参数不一致）")
    })
}

/// 把窗口放到虚拟桌面矩形上（物理像素）。
/// 用 Physical 坐标而不是 Logical：虚拟桌面跨不同缩放的显示器时，
/// 逻辑坐标换算由 Windows 决定，我们无法预期；而物理坐标是唯一
/// 能与「抓到的位图像素」严格对齐的坐标系。
fn place_window(win: &tauri::WebviewWindow, x: i32, y: i32, w: u32, h: u32) {
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
}

/// 后台编码线程：BGRA→RGBA → JPEG(q80) → base64 → FrameState → 事件推送。
/// 先 take 出 session 独占（像素量级 10-30MB，不 clone）；完成后归还，
/// 供选区按需裁剪（screenshot_select）。若期间会话已结束，直接清空。
///
/// `seq` 是会话号：**上一轮的编码线程晚到时不得回填位图**，否则会把旧屏
/// 塞回新一轮的会话，裁出来的是上一张截图。
fn spawn_encoder(app: tauri::AppHandle, seq: u64) {
    std::thread::spawn(move || {
        let Some(session) = screenshot::session().lock().unwrap().take() else {
            set_failed("截图会话不存在");
            return;
        };
        let payload: Result<FramePayload, String> = (|| {
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
                seq,
            })
        })();

        // 归还 session（会话仍在**且仍是同一轮**就留着给裁剪用，否则清空）。
        let alive = {
            let st = state().lock().unwrap();
            st.active && st.seq == seq
        };
        *screenshot::session().lock().unwrap() = alive.then_some(session);

        match payload {
            Ok(p) if alive => {
                state().lock().unwrap().frame = Some(FrameState::Ready(p.clone()));
                let _ = app.emit(FRAME_EVENT, p);
            }
            Ok(_) => {}
            Err(e) => set_failed(&e),
        }
    });
}

fn set_failed(error: &str) {
    let st = FrameState::Failed {
        error: error.to_string(),
    };
    state().lock().unwrap().frame = Some(st);
}

/// 会话兜底：30s 后会话仍在进行且窗口没亮（页面/编码异常）即报错收尾。
fn spawn_session_fallback(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(SESSION_FALLBACK_SECS));
        if !state().lock().unwrap().active {
            return; // 会话已正常结束
        }
        let visible = app
            .get_webview_window(OVERLAY_LABEL)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        if visible {
            return; // 页面已就绪，用户在正常交互
        }
        let _ = app.emit(
            "hermix:screenshot-error",
            "截图覆盖层 30s 未就绪，已关闭。若反复出现请重启应用。",
        );
        end_session(&app, false);
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
            if !state().lock().unwrap().active {
                break; // 会话已结束
            }
            let pressed = screenshot::esc_pressed();
            if pressed && !prev {
                end_session(&app, true);
                break;
            }
            prev = pressed;
        }
    });
}

/// 结束会话：清状态 + 隐藏覆盖窗（不销毁，下次复用）。
/// restore_focus = true 时把焦点还给截图前的前台窗口。
fn end_session(app: &tauri::AppHandle, restore_focus: bool) {
    let fg = {
        let mut st = state().lock().unwrap();
        st.frame = None;
        st.active = false;
        st.fg_hwnd.take()
    };
    *screenshot::session().lock().unwrap() = None;
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.hide();
    }
    if restore_focus {
        if let Some(h) = fg {
            screenshot::restore_foreground(h);
        }
    }
}

/// 覆盖窗查询幕布帧（页面挂载时拉一次，兜住"帧先到、页面后到"的时序）。
/// 之后的新帧一律走事件推送。
#[tauri::command]
pub fn screenshot_frame_data() -> Option<FrameState> {
    state().lock().unwrap().frame.clone()
}

/// 覆盖窗查询光标下窗口 rect（窗口吸附高亮）。
/// 入参：虚拟屏幕物理坐标。排除覆盖窗自身——全屏置顶窗不排除的话
/// 任何命中都是覆盖窗自己，吸附永远高亮整屏。
#[tauri::command]
pub fn screenshot_window_at(app: tauri::AppHandle, x: i32, y: i32) -> Option<screenshot::WindowRect> {
    let exclude = app
        .get_webview_window(OVERLAY_LABEL)
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    screenshot::window_rect_at((x, y), exclude)
}

/// 选区确认：裁出选区（无损 PNG data URL），并把覆盖窗缩到选区大小。
/// 返回裁剪结果；页面据此进入编辑态（只显示选中的那部分）。
#[tauri::command]
pub fn screenshot_select(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<String, String> {
    use crate::screenshot::Selection;
    // guard 必须绑定到局部变量活过整个函数体（锁临时值句尾释放会让 s 悬空）。
    let guard = screenshot::session().lock().unwrap();
    let s = guard
        .as_ref()
        .ok_or("截图会话已结束（幕布仍在编码或已被取消）")?;

    // 选区与位图求交集（越界部分截去），实际裁切尺寸以交集为准——
    // 窗口也跟着缩到交集大小，页面显示的和用户框的始终一致。
    let bx0 = s.origin_x;
    let by0 = s.origin_y;
    let bx1 = s.origin_x + s.width as i32;
    let by1 = s.origin_y + s.height as i32;
    let cx0 = x.max(bx0);
    let cy0 = y.max(by0);
    let cx1 = (x + width).min(bx1);
    let cy1 = (y + height).min(by1);
    let cw = cx1 - cx0;
    let ch = cy1 - cy0;
    if cw <= 0 || ch <= 0 {
        return Err("选区在屏幕之外，无法裁剪".into());
    }

    let rgba = screenshot::crop_rgba(
        s,
        Selection {
            x: cx0,
            y: cy0,
            width: cw,
            height: ch,
        },
    )
    .ok_or("选区在屏幕之外，无法裁剪")?;
    let png = screenshot::encode_png(&rgba, cw as u32, ch as u32)?;
    drop(guard);

    // 窗口缩到选区：编辑态只显示选中的那部分，底下没有黑底。
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        place_window(&win, cx0, cy0, cw as u32, ch as u32);
    }

    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// 页面回报「幕布已解码完毕」——此刻才亮窗，避免黑屏/半就绪画面。
#[tauri::command]
pub fn screenshot_overlay_ready(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
    }
    Ok(())
}

/// 编辑器提交：前端把编辑后的画布导出为 PNG data URL 回传，
/// 解码 → 剪贴板 → 收尾。选区裁剪已在前端画布完成，Rust 只收成品。
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
            end_session(&app, true);
            let _ = app.emit("hermix:screenshot-copied", ());
            Ok(())
        }
        Err(e) => {
            let _ = app.emit("hermix:screenshot-error", e.clone());
            Err(e)
        }
    }
}

/// 取消：清会话、隐藏覆盖窗。
#[tauri::command]
pub fn screenshot_cancel(app: tauri::AppHandle) -> Result<(), String> {
    end_session(&app, true);
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
