// 截图会话控制器：覆盖窗生命周期 + Tauri 命令。
// 数据通路见 screenshot.rs（抓屏/裁剪/剪贴板）、backdrop.rs（原生幕布）与
// docs/notes/implemented/feature/2026-10-23-screenshot-rewrite.md、
// docs/notes/implemented/feature/2026-10-24-screenshot-native-backdrop.md。
//
// 2026-10-23 结构重写（"打不开 / 慢 / 黑底 / 选多大不显示多大"四件套根修）：
// ① **覆盖窗只创建一次、反复复用**。原来每次截图都新建一个 WebView2
//    窗口（含页面加载），第二次起每次都白扔一次建窗 + 加载开销。现在
//    首开建窗，之后截图只是「定位 + 显示 + 发会话信息」。
// ② **亮窗时机交给页面**：页面把选区 UI 画完再回报 screenshot_overlay_ready，
//    Rust 才 show。窗口要么显示已就绪的画面，要么根本不出现，没有黑屏。
// ③ **选区即窗口**：选完区 Rust 把覆盖窗缩到选区大小，编辑器只显示
//    选中的那部分（原设计是全屏黑底 + 中间一小块图）。
// ④ **WebView2 参数单一来源**：覆盖窗不再单独传 additional_browser_args。
//    同一 user data folder 下所有 webview 的浏览器参数必须一致，不一致
//    建窗直接 0x8007139F（ERROR_INVALID_STATE）——此前主窗走 conf 的
//    参数、覆盖窗走另一份、进程环境变量还有第三份，三份不一致就是
//    "截图打不开"。现在统一由 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS 提供。
//
// 2026-10-24 幕布下沉原生（机制追平微信）：
// ⑤ 冻结帧不再进 webview——capture 的 BGRA 由 backdrop.rs 在主线程
//    UpdateLayeredWindow 直贴置顶分层窗（抓屏后 ~20-35ms@4K 上屏，
//    不编码不跨进程）。webview 改为**透明**，只画选区几何（吸附框/
//    选区/工具条）；它需要的一切就是一条几十字节的会话信息事件
//    （原点 + 尺寸 + seq），JPEG/base64/dataURL/编码线程整体删除。
// ⑥ 编辑态在编辑器渲染完成后由页面调 screenshot_backdrop_hide 收掉
//    幕布——编辑器画布与幕布是同一块像素，切换无缝，没有黑帧。

use crate::backdrop;
use crate::hotkey::{HotkeySlot, HotkeyStatus};
use crate::screenshot;
use base64::Engine as _;
use serde::Serialize;
use tauri::{Emitter, Manager};

const OVERLAY_LABEL: &str = "screenshot-overlay";
/// 会话信息事件名（页面订阅它开始渲染选区 UI）。
const SESSION_EVENT: &str = "hermix:screenshot-session";
/// 会话总兜底：正常以秒计，30s 没就绪即判异常收尾。
const SESSION_FALLBACK_SECS: u64 = 30;

/// 截图功能开关（设置 → 图像）。默认开；false 时热键与标题栏入口都拒绝。
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 截图热键槽。默认 Alt+Q；被别的程序占着就依次试备选键，
/// 并把"实际生效的键"回报给前端（详见 hotkey.rs 的三条坑注记）。
///
/// 备选键刻意避开系统保留组合（Ctrl+Alt+Del）、中文 IME 切换
/// （Ctrl+Shift+Space）与单键 F 键 —— 那些"看起来没人占"实则常被占用
/// 或被系统抢走。
static HOTKEY: HotkeySlot = HotkeySlot::new(
    "截图",
    "alt+Q",
    &["ctrl+Alt+A", "ctrl+Alt+S", "ctrl+Alt+J"],
);

/// 热键状态变化事件 —— 启动时占用兜底、前端改键后生效键变化都发它。
pub const HOTKEY_EVENT: &str = "hermix:screenshot-hotkey";

/// 覆盖窗建窗互斥锁（预热线程 vs 截图热键可能同时建窗）。
static OVERLAY_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 会话信息：覆盖窗渲染选区 UI 所需的全部数据（几十字节）。
/// 幕布像素不走这里——它在原生层直贴，页面永远不需要见到它。
#[derive(Serialize, Clone)]
pub struct SessionInfo {
    /// 位图原点（虚拟屏幕坐标，物理像素）——覆盖窗把本地坐标换算回去用
    origin_x: i32,
    origin_y: i32,
    /// 物理像素尺寸（= 虚拟桌面尺寸）
    width: u32,
    height: u32,
    /// 会话序号：页面据此丢弃上一轮残留的迟到事件
    seq: u64,
}

/// 会话状态机：None = 准备中（页面等事件）；Ready = 渲染 UI 并 show；
/// Failed = 页面显示错误 UI（ESC / Rust 兜底监视仍可退出）。
/// untagged 序列化：Ready 平铺为 SessionInfo，Failed 为 {"error": …}。
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum SessionState {
    Ready(SessionInfo),
    Failed { error: String },
}

/// 覆盖窗会话状态（全局单例）。
struct OverlayState {
    /// 当前会话信息（None = 准备中）
    session: Option<SessionState>,
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
            session: None,
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

/// 会话主流程：抓屏 ~10ms → 定位窗口 → 主线程 ULW 直贴幕布 → 发会话信息。
/// 感知「出画面」= 抓屏 + ULW 直贴（4K ~20-35ms），不经过任何编码/序列化。
pub fn run_session(app: &tauri::AppHandle) -> Result<(), String> {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("截图功能已在设置 → 图像中关闭".into());
    }
    // 重入：结束上一轮（覆盖窗复用，不销毁）。
    end_session(app, false);

    let t0 = std::time::Instant::now();
    // 一次抓整块虚拟桌面：多显示器一次覆盖，屏间无黑缝。
    let session = screenshot::capture_desktop()?;
    let capture_ms = t0.elapsed().as_millis();
    let (ox, oy, w, h) = (
        session.origin_x,
        session.origin_y,
        session.width,
        session.height,
    );
    let fg = screenshot::foreground_window();
    // 全分辨率缓冲留给选区裁剪（screenshot_select），幕布绘制按引用读。
    *screenshot::session().lock().unwrap() = Some(session);

    // 覆盖窗：首次创建，之后复用（省掉 WebView2 建窗 + 页面加载）。
    let win = ensure_overlay(app)?;
    place_window(&win, ox, oy, w, h);

    {
        let mut st = state().lock().unwrap();
        st.session = None;
        st.active = true;
        st.seq += 1;
        st.fg_hwnd = fg;
    }
    let seq = state().lock().unwrap().seq;

    spawn_esc_watcher(app.clone());
    spawn_session_fallback(app.clone());

    // 原生幕布：主线程 ULW 直贴（tao 事件泵为该窗派发消息，Win32 调用
    // 必须在主线程）。paint 期间主线程停 5-15ms——一次性成本，与微信
    // 在 UI 线程画冻结帧同款。像素从 session 缓冲按引用读，不拷贝。
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let t1 = std::time::Instant::now();
        let guard = screenshot::session().lock().unwrap();
        let painted = match guard.as_ref() {
            Some(s) => backdrop::paint_and_show(s.origin_x, s.origin_y, s.width, s.height, &s.pixels),
            // 闭包排队期间会话被取消（ESC）：无事可做。
            None => Ok(()),
        };
        drop(guard);
        if let Err(e) = painted {
            set_failed(&app2, &format!("原生幕布绘制失败: {e}"));
            return;
        }
        // 幕布 TOPMOST 插队把 webview 压到了下面；抬回去（选区 UI 必须在幕布上）。
        if let Some(wv) = app2.get_webview_window(OVERLAY_LABEL) {
            let _ = wv.set_always_on_top(true);
        }
        tracing::info!(
            "[screenshot] capture={capture_ms}ms backdrop={}ms",
            t1.elapsed().as_millis()
        );
    })
    .map_err(|e| format!("幕布任务派发失败: {e}"))?;

    // 会话信息先发（几十字节，页面立即渲染选区 UI 并回报 ready → 亮 webview）。
    // 幕布不等它——冻结画面在 paint 闭包里独立上屏。
    let info = SessionInfo {
        origin_x: ox,
        origin_y: oy,
        width: w,
        height: h,
        seq,
    };
    state().lock().unwrap().session = Some(SessionState::Ready(info.clone()));
    let _ = app.emit(SESSION_EVENT, info);

    Ok(())
}

/// 取/建覆盖窗。已存在就复用——这是"第二次起快一个数量级"的关键。
///
/// 建窗是**原子的**：预热线程（启动 6s 后）与截图热键可能同时触发，
/// 两个同名窗口建出来第二个会失败（label 冲突）→ 截图直接报错。
/// 抢锁 + 二次检查：等锁期间别人可能已经建好。
fn ensure_overlay(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        return Ok(win);
    }
    let _guard = OVERLAY_INIT.lock().unwrap();
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
    // **透明是承重假设**：幕布在原生分层窗（backdrop.rs），webview 必须
    // 透出底下的冻结画面，它自己只画选区几何。DefaultBackgroundColor
    // alpha=0 由 Tauri 落到 WebView2。
    .transparent(true)
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

fn set_failed(app: &tauri::AppHandle, error: &str) {
    let st = SessionState::Failed {
        error: error.to_string(),
    };
    state().lock().unwrap().session = Some(st.clone());
    let _ = app.emit(SESSION_EVENT, st);
}

/// 会话兜底：30s 后会话仍在进行且窗口没亮（页面/幕布异常）即报错收尾。
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

/// 结束会话：清状态 + 隐藏覆盖窗与幕布（都不销毁，下次复用）。
/// restore_focus = true 时把焦点还给截图前的前台窗口。
fn end_session(app: &tauri::AppHandle, restore_focus: bool) {
    let fg = {
        let mut st = state().lock().unwrap();
        st.session = None;
        st.active = false;
        st.fg_hwnd.take()
    };
    *screenshot::session().lock().unwrap() = None;
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.hide();
    }
    let _ = app.run_on_main_thread(backdrop::hide);
    if restore_focus {
        if let Some(h) = fg {
            screenshot::restore_foreground(h);
        }
    }
}

/// 覆盖窗查询会话信息（页面挂载时拉一次，兜住"事件先到、页面后到"的时序）。
/// 之后的新会话一律走事件推送。
#[tauri::command]
pub fn screenshot_session_info() -> Option<SessionState> {
    state().lock().unwrap().session.clone()
}

/// 覆盖窗查询光标下窗口 rect（窗口吸附高亮）。
/// 入参：虚拟屏幕物理坐标。排除覆盖窗与原生幕布窗——两个全屏置顶窗
/// 不排除的话，任何命中都是它们自己，吸附永远高亮整屏。
#[tauri::command]
pub fn screenshot_window_at(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
) -> Option<screenshot::WindowRect> {
    let mut exclude = Vec::with_capacity(2);
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        if let Ok(h) = w.hwnd() {
            exclude.push(h.0 as isize);
        }
    }
    if let Some(h) = backdrop::hwnd() {
        exclude.push(h);
    }
    screenshot::window_rect_at((x, y), &exclude)
}

/// 选区确认：裁出选区（无损 PNG data URL），并把覆盖窗缩到选区大小。
/// 返回裁剪结果；页面据此进编辑态，编辑器渲染完成后由页面收幕布
/// （screenshot_backdrop_hide）——顺序保证切换无黑帧。
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
        .ok_or("截图会话已结束（幕布已收起或被取消）")?;

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

/// 页面回报「选区 UI 已画完」——此刻才亮覆盖窗（幕布上屏在幕布闭包里，
/// 独立于本命令）。避免黑屏/半就绪画面。
#[tauri::command]
pub fn screenshot_overlay_ready(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
    }
    Ok(())
}

/// 编辑器已用选区像素渲染完成 → 收掉原生幕布，回到活屏幕。
/// 编辑器画布与幕布是同一块像素，切换无缝；幕布若已被取消流程收掉，
/// 本命令幂等。
#[tauri::command]
pub fn screenshot_backdrop_hide(app: tauri::AppHandle) -> Result<(), String> {
    let _ = app.run_on_main_thread(backdrop::hide);
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
///
/// 顺序是"先注册新的、成功后再解旧的"——注册失败（被占用）时旧热键
/// 仍在，现状不被破坏。被占且备选键也全被占才报错。
///
/// 返回**真正生效**的热键状态：请求的键被占时 `substituted = true`，
/// `active` 是替上去的备选键（前端据此提示"已改用 X"，而不是假装失败）。
#[tauri::command]
pub fn screenshot_apply_config(
    app: tauri::AppHandle,
    enabled: bool,
    hotkey: String,
) -> Result<HotkeyStatus, String> {
    ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    if !enabled {
        // 关闭：解掉当前热键（含 setup 默认注册的兜底）。
        HOTKEY.disable(&app);
        let _ = app.emit(HOTKEY_EVENT, HotkeyStatus::default());
        return Ok(HotkeyStatus::default());
    }

    let status = HOTKEY.apply(&app, &hotkey)?;
    if status.substituted {
        eprintln!(
            "[screenshot] 热键 {hotkey} 被占用，已改用 {}",
            status.active.as_deref().unwrap_or("?")
        );
    }
    let _ = app.emit(HOTKEY_EVENT, status.clone());
    Ok(status)
}

/// 查询截图热键当前状态（设置页打开时读一次，显示真正生效的键）。
#[tauri::command]
pub fn screenshot_hotkey_status() -> HotkeyStatus {
    HOTKEY.status()
}

/// 热键是否属于截图（lib.rs 的全局派发用它）。
pub fn hotkey_matches(shortcut: &tauri_plugin_global_shortcut::Shortcut) -> bool {
    HOTKEY.matches(shortcut)
}

/// setup 兜底：注册默认热键。被占则换备选键，并把结果告诉前端。
pub fn register_default_hotkey(app: &tauri::AppHandle) -> HotkeyStatus {
    let status = HOTKEY.apply_default(app);
    if status.substituted {
        let _ = app.emit(HOTKEY_EVENT, status.clone());
    }
    status
}

/// 预热覆盖窗与幕布窗 —— 提前把 WebView2 窗口和原生幕布窗建好。
///
/// 首次截图原本要付「建 WebView2 窗口 + 加载页面」的开销，用户感知就是
/// "按了热键，等一会儿才出画布"。窗口建好后每次截图只剩
/// 「定位 + ULW 直贴 + 发会话信息」，感知延迟与微信同一量级。
///
/// 刻意**不在启动关键路径上建窗**：两个 WebView2 抢 CPU 会拖慢首屏。
/// 调用方在主窗口就绪后再触发（见 lib.rs）。
pub fn prewarm(app: &tauri::AppHandle) {
    let inner = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(e) = ensure_overlay(&inner) {
            eprintln!("[screenshot] 覆盖窗预热失败: {e}");
        }
        if let Err(e) = ensure_backdrop() {
            eprintln!("[screenshot] 幕布窗预热失败: {e}");
        }
    });
}

/// 幕布窗预热（幂等；失败不致命，首帧 paint 会自愈重试）。
fn ensure_backdrop() -> Result<(), String> {
    backdrop::ensure_created()
}
