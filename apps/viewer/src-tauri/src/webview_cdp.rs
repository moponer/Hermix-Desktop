//! 内嵌网页视图的 **agent 操控面**（通道 A 的可驱动版）。
//!
//! 背景：查看器里的网页（`webview_host.rs` 的子 webview）一直是**给人看的**
//! ——agent 没有操控通道，于是 agent 干活时只能去开一个系统浏览器（通道 B），
//! 页面就甩到应用外面去了。
//!
//! 这里补上：给子 webview 的 WebView2 进程开一个调试端口，agent 就能**直接
//! 驱动用户正在看的那块预览区**——同一块屏幕、同一个页面、同一个登录态。
//!
//! 端口**不写死**：`--remote-debugging-port=0` → 浏览器进程把真实端口写进
//! profile 里的 `DevToolsActivePort`（hermix-tools 的 F1 教训：写死端口会撞
//! 用户自己的浏览器）。子 webview 用**独立的 WebView2 数据目录** → 它就是
//! 独立的浏览器进程 → 端口天然跟主窗口隔离。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cdp::{evaluate, Cdp};

/// 等调试端口就绪的上限。
const PORT_TIMEOUT: Duration = Duration::from_secs(10);

/// 内嵌网页视图的 WebView2 数据目录（与 `webview_host` 共用）。
fn profile_dir() -> PathBuf {
    hermix_config::paths::hermix_home().join("webview-profile")
}

/// 读 `DevToolsActivePort` → (端口, ws 路径)。端口没就绪就等一会儿。
///
/// **路径要试两处**：WebView2 把用户数据放在 UDF 的 `EBWebView` 子目录里
/// （`DevToolsActivePort` 在那里面），而系统浏览器（Edge/Chrome）是把文件
/// 直接写在 UDF 根下。两处都找，才不会因为引擎不同而读不到端口。
fn devtools_endpoint() -> Result<(u16, String), String> {
    let base = profile_dir();
    let candidates = [base.join("DevToolsActivePort"), base.join("EBWebView").join("DevToolsActivePort")];
    let deadline = Instant::now() + PORT_TIMEOUT;
    while Instant::now() < deadline {
        for file in &candidates {
            if let Ok(text) = std::fs::read_to_string(file) {
                let mut lines = text.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                    if let Ok(port) = port.trim().parse::<u16>() {
                        if !path.trim().is_empty() {
                            return Ok((port, path.trim().to_string()));
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Err("内嵌网页视图的调试端口还没就绪（视图是否已打开？）".to_string())
}

/// 连上内嵌网页视图的页面 target，返回 (连接, 会话 id)。
fn connect_page() -> Result<(Cdp, String), String> {
    let (port, ws_path) = devtools_endpoint()?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    // 找页面 target：类型是 page 的那个（内嵌视图只有一个页面）。
    let targets = cdp.targets()?;
    let page = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .ok_or_else(|| "内嵌网页视图里没有找到页面 target".to_string())?;
    let target_id = page
        .get("targetId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "页面 target 没有 targetId".to_string())?
        .to_string();
    let sid = cdp.attach(&target_id)?;
    Ok((cdp, sid))
}

/// 当前页面的状态。
#[derive(serde::Serialize, Clone, Debug)]
pub struct WebViewState {
    pub url: String,
    pub title: String,
}

/// 页面状态（URL / 标题）。
pub fn state() -> Result<WebViewState, String> {
    let (mut cdp, sid) = connect_page()?;
    let url = evaluate(&mut cdp, &sid, "location.href").unwrap_or_default();
    let title = evaluate(&mut cdp, &sid, "document.title").unwrap_or_default();
    Ok(WebViewState { url, title })
}

/// 页面可见文本（比截图省 token，agent 读页面首选）。
pub fn text() -> Result<String, String> {
    let (mut cdp, sid) = connect_page()?;
    let raw = evaluate(
        &mut cdp,
        &sid,
        "(document.body && document.body.innerText) || ''",
    )?;
    Ok(crate::mcp_server::truncate_chars(&raw, 6000))
}

/// 执行一段 JS，返回按值的结果。
///
/// `frame` = 目标 iframe 的 URL 片段（可选）。**跨源 iframe 里的元素顶层
/// 够不到**（超星的视频播放器就在 `/ananas/modules/video/index.html` 这个
/// 跨源 iframe 里）→ 指定 frame 时改在那个 iframe 的执行上下文里跑。
pub fn eval(expression: &str, frame: Option<&str>) -> Result<String, String> {
    let (mut cdp, sid) = connect_page()?;
    match frame.map(str::trim).filter(|f| !f.is_empty()) {
        Some(needle) => cdp.evaluate_in_frame(&sid, needle, expression),
        None => evaluate(&mut cdp, &sid, expression),
    }
}

/// 点击页面坐标（CSS 像素，相对视口左上角）。
pub fn click(x: f64, y: f64) -> Result<String, String> {
    let (mut cdp, sid) = connect_page()?;
    for pressed in [true, false] {
        cdp.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": if pressed { "mousePressed" } else { "mouseReleased" },
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1
            }),
            Some(&sid),
        )?;
    }
    std::thread::sleep(Duration::from_millis(250));
    Ok(format!("已点击 ({x}, {y})"))
}

/// **移动鼠标**到页面坐标（悬停）。
///
/// 有些站点的菜单是**悬停才出现**的（比如超星的用户名 →「个人空间 / 进入
/// 空间」）。JS 派发的合成 mouseover 不一定能触发它们的处理器；CDP 的
/// mouseMoved 是**真鼠标移动**，浏览器会照常派发 hover / mouseenter。
pub fn hover(x: f64, y: f64) -> Result<String, String> {
    let (mut cdp, sid) = connect_page()?;
    cdp.call(
        "Input.dispatchMouseEvent",
        json!({"type": "mouseMoved", "x": x, "y": y}),
        Some(&sid),
    )?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(format!("鼠标已移到 ({x}, {y})"))
}

/// 截图：返回 (落盘路径, 缩略图 base64, mime)。
pub fn screenshot() -> Result<(String, String, String), String> {
    let (mut cdp, sid) = connect_page()?;
    let result = cdp.call(
        "Page.captureScreenshot",
        json!({"format": "png", "captureBeyondViewport": false}),
        Some(&sid),
    )?;
    let data = result
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| "内嵌网页视图没有返回截图数据".to_string())?;
    let png = crate::commands::base64_decode(data)
        .ok_or_else(|| "截图数据不是合法 base64".to_string())?;
    crate::cdp::save_and_thumbnail(&png, "in-app-webview")
}

/// 在内嵌网页视图里跳转。
pub fn navigate(url: &str) -> Result<String, String> {
    let target = crate::webview_host::normalize_url(url)?;
    let (mut cdp, sid) = connect_page()?;
    cdp.call("Page.navigate", json!({ "url": target.as_str() }), Some(&sid))?;
    Ok(target.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_port_file_reports_clearly() {
        // 没有打开内嵌视图时，端口文件不存在 → 必须给一句能看懂的错误，
        // 而不是 panic 或空串。
        let err = devtools_endpoint().unwrap_err();
        assert!(err.contains("调试端口"), "实际错误：{err}");
    }
}
