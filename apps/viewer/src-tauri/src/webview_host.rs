//! 内嵌网页视图（**通道 A**）：把远程网页渲染在**主窗口内部**。
//!
//! 为什么不是 iframe：① 我们的 CSP 是 `default-src 'self'`，`frame-src` 不放行
//! 外部站点；② 绝大多数站点发 `X-Frame-Options: DENY` / `frame-ancestors` 直接
//! 拒嵌。用顶层子 webview 两道都绕开。
//!
//! 为什么不是系统浏览器（那是**通道 B**，见 `browser.rs`）：用户要的是
//! 「在预览界面展示」，系统浏览器会把人整个带出应用。
//!
//! 安全边界（第一准则）：
//!   - 只放行 http / https（`javascript:` / `file:` / `ms-msdt:` 一律拒），
//!     页面内跳转同样受限（`on_navigation`）——这是 WebView2 已知攻击面的
//!     入口（`ms-msdt:` 之类）。
//!   - 子 webview 与**主窗口共用同一份** WebView2 user data folder：同一进程里
//!     WebView2 不允许分叉（另设 `data_directory` 会报 0x8007139F），隔离靠的是
//!     Chromium 按源划分 cookie / localStorage + 子 webview 不配置 IPC
//!     （capabilities 只覆盖 main）→ 远程站点读写不到主窗口的数据。
//!   - capabilities 只覆盖 main 窗口 → 子 webview 拿不到我们的 IPC，
//!     远程页面无法调用本地命令（也不配 `remote.urls`）。
//!
//! **已知取舍**：wry 默认关掉 SmartScreen
//! （`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`），
//! 而 Chromium 的 disable 列表**优先于** enable 列表 → 无法在子 webview 里
//! 单独恢复。补偿手段是上面三条（独立 profile / 无 IPC / 协议白名单）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::webview::WebviewBuilder;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Rect, Webview, WebviewUrl,
};

/// 子 webview 的 label（主窗口内唯一）。
const WEB_LABEL: &str = "webview";
/// 主窗口 label。
const MAIN_LABEL: &str = "main";

/// 查看器的 WebView2 用户数据目录（**唯一出处**，主窗口与子 webview 共用）。
///
/// 读环境变量而不是每次重算路径：`prepare_user_data_folder()` 在 `main()` 里
/// 已经钉死它，读回来才能保证「谁用谁一致」（CDP 端口文件也在这个目录下）。
pub fn user_data_folder() -> PathBuf {
    std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")
        .map(PathBuf::from)
        .unwrap_or_else(default_user_data_folder)
}

fn default_user_data_folder() -> PathBuf {
    hermix_config::paths::hermix_home().join("viewer-webview")
}

/// 启动时把 `WEBVIEW2_USER_DATA_FOLDER` 钉到查看器自己的目录（幂等）。
///
/// **必须在第一个 webview 创建之前调用**：WebView2 在创建第一个环境时才读这个
/// 变量，之后再设等于没设。
///
/// 为什么要覆盖父进程传进来的值：桌面主程序把它自己的 `webview-cache` 设进了
/// 环境变量，子进程（本查看器）会原样继承。若不覆盖，查看器主窗口就和桌面主
/// 窗口抢同一份用户数据（跨进程锁冲突），而子 webview 无论设不设
/// `data_directory` 都会失败（`0x8007139F` / `ERROR_INVALID_STATE`）。
pub fn prepare_user_data_folder() -> PathBuf {
    let udf = default_user_data_folder();
    let _ = std::fs::create_dir_all(&udf);
    std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &udf);
    udf
}

/// 网址归一 + 协议白名单：只放行 http / https。
pub fn normalize_url(raw: &str) -> Result<tauri::Url, String> {
    let trimmed = raw.trim();
    let candidate = if trimmed.starts_with("www.") {
        format!("https://{trimmed}")
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        return Err("只支持 http / https 网址".to_string());
    };
    let url = tauri::Url::parse(&candidate).map_err(|e| format!("网址无法解析：{e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!("不支持的协议：{other}")),
    }
}

/// 找当前活动的子 webview。
fn active_webview(app: &AppHandle) -> Option<Webview> {
    let window = app.get_window(MAIN_LABEL)?;
    window
        .webviews()
        .into_iter()
        .find(|w| w.label() == WEB_LABEL)
}

/// 关掉当前子 webview（没有就什么都不做）。
pub fn close_webview(app: &AppHandle) {
    if let Some(webview) = active_webview(app) {
        let _ = webview.close();
    }
}

/// 显示 / 隐藏当前子 webview。
///
/// **为什么菜单展开时必须把它藏起来**：网页是**独立的 HWND**（子 webview），
/// 它永远画在父 webview 的 DOM **之上** —— 不是同一层的东西，CSS 的
/// z-index 根本管不到它。于是标题栏下拉菜单 / 右键菜单一旦展开到内容区
/// 的范围，就被这块网页整个盖住（"菜单被内容区域遮挡"）。
///
/// 菜单收起后要**恢复显示**：直接关掉会丢页面状态（重新加载、滚动位置、
/// 表单内容全没了），hide/show 只动可见性，页面原样还在。
#[tauri::command]
pub fn web_set_visible(app: AppHandle, visible: bool) {
    if let Some(webview) = active_webview(&app) {
        let _ = if visible {
            webview.show()
        } else {
            webview.hide()
        };
    }
}

/// 打开网页：新建（或替换）子 webview。
///
/// **必须在非主线程调用**：Windows 上 WebView2 的环境创建在主线程调用会死锁
/// （wry#583）。Tauri 的异步命令跑在 tokio 工作线程上，安全。
#[tauri::command]
pub async fn web_open(
    app: AppHandle,
    url: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    open_sync(&app, &url, x, y, w, h)
}

/// 建视图的串行闸。
///
/// MCP 是 HTTP 端点，**每条连接一个线程**：两个客户端同时 `web_open` 会对同一个
/// label 并发 `add_child` → WebView2 环境创建与 UI 线程互相等死（用户看到的是
/// 「界面正忙」，整个查看器不可用，比"视图开不了"严重得多）。
static OPEN_LOCK: Mutex<()> = Mutex::new(());

/// 真正开视图的同步实现（异步命令与 MCP 工具共用）。
///
/// MCP 工具也在非主线程上跑（HTTP 端点每条连接一个线程），所以同样安全。
///
/// 三条纪律（2026-10-30）：**串行**（见 `OPEN_LOCK`）、**幂等**（同一个 URL 已经
/// 在看就不动它）、**等旧视图真的销毁再建**（`close()` 是异步的，同 label 抢先
/// 重建会 `ERROR_INVALID_STATE`）。
pub fn open_sync(
    app: &AppHandle,
    url: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let target = normalize_url(url)?;
    let _guard = OPEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 幂等：同一个页面不销毁重建 —— 重建会丢登录态、滚动位置和表单内容，
    // 而 `web_open` 在自动化流程里会被反复调用（每轮导航前一次）。
    if let Some(webview) = active_webview(app) {
        let same = webview
            .url()
            .map(|u| u.as_str() == target.as_str())
            .unwrap_or(false);
        if same {
            remember_bounds(x, y, w, h);
            return Ok(());
        }
    }

    close_webview(app);
    // `Webview::close()` 只是发销毁请求，`window.webviews()` 要等 Destroyed 才更新。
    let deadline = Instant::now() + Duration::from_secs(1);
    while active_webview(app).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    let emitter = app.clone();
    let builder = WebviewBuilder::new(WEB_LABEL, WebviewUrl::External(target))
        // **不开独立 data_directory**：同一进程里所有 WebView2 必须共用同一个
        // user data folder，子 webview 若另设 data_directory 会报
        // 0x8007139F（ERROR_INVALID_STATE）。主窗口用的是启动时固定的
        // WEBVIEW2_USER_DATA_FOLDER（查看器自己的目录），子 webview 继承同一份即可。
        // **开调试端口**（端口写 0 = 让浏览器进程自己挑，真实端口写进 profile 的
        // DevToolsActivePort）：这是 agent 能驱动「用户正在看的那块预览区」的
        // 关键 —— 用户与 agent 看到的是同一个页面、同一个登录态。
        .additional_browser_args("--remote-debugging-port=0")
        // 页面内跳转同样只放行 http / https。
        .on_navigation(|url| matches!(url.scheme(), "http" | "https"))
        // 页面自己跳了别的地址 → 通知前端（标签标题要跟着变）。
        .on_page_load(move |_webview, payload| {
            let _ = emitter.emit("web-navigated", payload.url().to_string());
        });

    let window = app
        .get_window(MAIN_LABEL)
        .ok_or_else(|| "主窗口不存在".to_string())?;
    window
        .add_child(
            builder,
            PhysicalPosition::new(x as i32, y as i32),
            PhysicalSize::new(w.max(1.0) as u32, h.max(1.0) as u32),
        )
        .map_err(|e| create_error(&e))?;
    remember_bounds(x, y, w, h);
    // 告诉前端「有个网页视图开了」→ 前端开对应的网页标签（否则用户看到的是
    // 一块没有标签的网页浮在界面上）。
    let _ = app.emit("web-opened", url.to_string());
    Ok(())
}

/// 建视图失败的错误体：带足够事实，让调用方不必再猜。
///
/// 2026-10-30 之前的错误只有 `内嵌网页视图创建失败：{HRESULT}`，于是同一类故障
/// 被反复误判成"UDF 名字不对 / 内核不对 / 拉起方式不对"。
fn create_error(err: &dyn std::fmt::Display) -> String {
    let text = err.to_string();
    let hint = if text.contains("0x8007139F") || text.contains("8007139F") {
        "本进程存在第二套 WebView2 用户数据目录：检查是否给子视图设了 data_directory，\
         或别处改写了 WEBVIEW2_USER_DATA_FOLDER（一个进程只允许一套 UDF）"
    } else if text.contains("0x80070005") || text.contains("拒绝访问") {
        "UDF 目录被另一个进程占用：确认没有第二个查看器进程（不要删 profile，里面是登录态）"
    } else {
        "先稍后重试一次；仍失败就重启查看器（不要删 profile，里面是登录态）"
    };
    format!(
        "内嵌网页视图创建失败：{text} | code=create_failed | udf={} | hint={hint}",
        user_data_folder().display()
    )
}

/// 最近一次的视图位置（物理像素）——agent 侧开视图时用它摆位。
fn last_bounds() -> (f64, f64, f64, f64) {
    bounds_store()
        .lock()
        .unwrap()
        .unwrap_or((0.0, 0.0, 900.0, 700.0))
}

/// 记住视图位置。
fn remember_bounds(x: f64, y: f64, w: f64, h: f64) {
    *bounds_store().lock().unwrap() = Some((x, y, w, h));
}

/// 视图位置（物理像素）。
static BOUNDS: OnceLock<Mutex<Option<(f64, f64, f64, f64)>>> = OnceLock::new();

fn bounds_store() -> &'static Mutex<Option<(f64, f64, f64, f64)>> {
    BOUNDS.get_or_init(|| Mutex::new(None))
}

/// 应用句柄（登记在 `viewer_control`，这里是转发）。
pub fn set_app(app: AppHandle) {
    crate::viewer_control::set_app(app);
}

/// 取应用句柄。
pub fn app() -> Result<AppHandle, String> {
    crate::viewer_control::app()
}

/// 用**上次的位置**开一个网址（给 agent 用：它不知道预览区在哪）。
pub fn open_at_last_bounds(url: &str) -> Result<(), String> {
    let app = app()?;
    let (x, y, w, h) = last_bounds();
    open_sync(&app, url, x, y, w, h)
}

/// 位置 / 尺寸同步（前端在窗口缩放、面板开合时调用）。
///
/// 参数是**物理像素**：子 webview 是独立 HWND，CSS 像素定位会随 DPI 偏。
#[tauri::command]
pub fn web_bounds(app: AppHandle, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let Some(webview) = active_webview(&app) else {
        return Ok(());
    };
    let rect = Rect {
        position: PhysicalPosition::new(x as i32, y as i32).into(),
        size: PhysicalSize::new(w.max(1.0) as u32, h.max(1.0) as u32).into(),
    };
    webview
        .set_bounds(rect)
        .map_err(|e| format!("网页视图位置更新失败：{e}"))
}

/// 关掉网页视图（前端离开网页标签时调用：否则子窗口会浮在应用上面）。
#[tauri::command]
pub fn web_close(app: AppHandle) {
    close_webview(&app);
}

/// 当前内嵌视图的地址。
///
/// CDP 那边要在「浏览器进程的多个 page target」里挑出**当前这个**（超星页 +
/// about:blank 残留页）→ 用这个地址精确匹配。
pub fn active_webview_url() -> Option<String> {
    let app = app().ok()?;
    let webview = active_webview(&app)?;
    webview.url().ok().map(|u| u.to_string())
}

/// 在当前网页视图里跳转。
#[tauri::command]
pub fn web_navigate(app: AppHandle, url: String) -> Result<(), String> {
    let target = normalize_url(&url)?;
    let Some(webview) = active_webview(&app) else {
        return Err("没有活动的网页视图".to_string());
    };
    let literal = serde_json::to_string(target.as_str()).unwrap_or_else(|_| "\"\"".to_string());
    webview
        .eval(format!("window.location.href = {literal};"))
        .map_err(|e| format!("网页跳转失败：{e}"))
}

/// 在当前网页视图里执行一段脚本（前进 / 后退 / 刷新这类浏览器动作）。
#[tauri::command]
pub fn web_exec(app: AppHandle, script: String) -> Result<(), String> {
    let Some(webview) = active_webview(&app) else {
        return Err("没有活动的网页视图".to_string());
    };
    webview
        .eval(script)
        .map_err(|e| format!("网页脚本执行失败：{e}"))
}

/// 当前网页视图的状态（前端拿它决定要不要显示占位）。
#[derive(serde::Serialize)]
pub struct WebState {
    pub open: bool,
    pub url: String,
}

#[tauri::command]
pub fn web_state(app: AppHandle) -> WebState {
    match active_webview(&app) {
        Some(webview) => WebState {
            open: true,
            url: webview.url().map(|u| u.to_string()).unwrap_or_default(),
        },
        None => WebState {
            open: false,
            url: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn only_http_and_https_are_allowed() {
        assert!(normalize_url("https://example.com").is_ok());
        assert!(normalize_url("http://example.com/a?b=1").is_ok());
        // www 前缀补协议
        assert_eq!(
            normalize_url("www.example.com").unwrap().as_str(),
            "https://www.example.com/"
        );
        // 危险协议一律拒
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("file:///C:/Windows/System32").is_err());
        assert!(normalize_url("ms-msdt:/id").is_err());
        assert!(normalize_url("C:\\Windows").is_err());
    }
}
