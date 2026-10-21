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
//!   - 子 webview 走**独立的 WebView2 数据目录** → 远程站点的 cookie /
//!     localStorage 不落进主窗口的 profile，两次启动之间也不残留。
//!   - capabilities 只覆盖 main 窗口 → 子 webview 拿不到我们的 IPC，
//!     远程页面无法调用本地命令（也不配 `remote.urls`）。
//!
//! **已知取舍**：wry 默认关掉 SmartScreen
//! （`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`），
//! 而 Chromium 的 disable 列表**优先于** enable 列表 → 无法在子 webview 里
//! 单独恢复。补偿手段是上面三条（独立 profile / 无 IPC / 协议白名单）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tauri::webview::WebviewBuilder;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Rect, Webview, WebviewUrl,
};

/// 子 webview 的 label（主窗口内唯一）。
const WEB_LABEL: &str = "webview";
/// 主窗口 label。
const MAIN_LABEL: &str = "main";

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

/// 子 webview 的独立数据目录（与主窗口 profile 隔离）。
fn profile_dir() -> PathBuf {
    hermix_config::paths::hermix_home().join("webview-profile")
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

/// 真正开视图的同步实现（异步命令与 MCP 工具共用）。
///
/// MCP 工具也在非主线程上跑（HTTP 端点每条连接一个线程），所以同样安全。
pub fn open_sync(
    app: &AppHandle,
    url: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let target = normalize_url(url)?;
    close_webview(app);

    let emitter = app.clone();
    let builder = WebviewBuilder::new(WEB_LABEL, WebviewUrl::External(target))
        // 独立 profile：远程站点不留痕，也不与主窗口共享 cookie。
        .data_directory(profile_dir())
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
        .map_err(|e| format!("内嵌网页视图创建失败：{e}"))?;
    remember_bounds(x, y, w, h);
    // 告诉前端「有个网页视图开了」→ 前端开对应的网页标签（否则用户看到的是
    // 一块没有标签的网页浮在界面上）。
    let _ = app.emit("web-opened", url.to_string());
    Ok(())
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
