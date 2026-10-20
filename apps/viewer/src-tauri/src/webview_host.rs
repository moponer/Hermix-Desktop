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
    let target = normalize_url(&url)?;
    close_webview(&app);

    let emitter = app.clone();
    let builder = WebviewBuilder::new(WEB_LABEL, WebviewUrl::External(target))
        // 独立 profile：远程站点不留痕，也不与主窗口共享 cookie。
        .data_directory(profile_dir())
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
    Ok(())
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
