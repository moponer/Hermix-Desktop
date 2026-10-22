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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

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
    devtools_endpoint_in(&profile_dir(), PORT_TIMEOUT)
}

/// 同上，但**基目录与超时都显式传入** —— 测试要用一个空的临时目录，
/// 否则本机只要开着内嵌视图，真实 profile 里就有 `DevToolsActivePort`，
/// 测试的前提（"没开视图"）根本不成立。
fn devtools_endpoint_in(base: &Path, timeout: Duration) -> Result<(u16, String), String> {
    let candidates = [
        base.join("DevToolsActivePort"),
        base.join("EBWebView").join("DevToolsActivePort"),
    ];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for file in &candidates {
            if let Ok(text) = std::fs::read_to_string(file) {
                let mut lines = text.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                    if let Ok(port) = port.trim().parse::<u16>() {
                        if !path.trim().is_empty() {
                            // **文件存在不等于端口活着。**
                            //
                            // WebView2 把端口写进 UDF 后从不清理：进程退出、或
                            // 复用已有环境（此时 `--remote-debugging-port` 不再
                            // 生效）时，这个文件会留着上一次的值。直接用它去
                            // 连接，报出来的是"连接被拒"，而真正的原因是"这个
                            // 端口是上一世的"。
                            //
                            // 所以先握手再采信；不通就继续等——新环境起来后会
                            // 覆盖这个文件。
                            if port_is_live(port) {
                                return Ok((port, path.trim().to_string()));
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    // 错误里带上现场，省掉一整轮排查：用户/agent 需要知道"目录在不在、环境起没起"，
    // 而不是只有一句"端口没就绪"。
    let initialised = base.join("EBWebView").is_dir();
    Err(format!(
        "内嵌网页视图的调试端口还没就绪（视图是否已打开？）\n\
         数据目录：{}\n\
         （目录存在={}，WebView2 环境已初始化={initialised}）\n\
         若视图确实已打开却一直读不到这个文件：说明 WebView2 的浏览器进程没带 \
         `--remote-debugging-port` 启动。查看器的 `web_open` 会设置 \
         WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS，故重启查看器后再试；\
         仍不行则是有残留 msedgewebview2 锁着该目录。",
        base.display(),
        base.is_dir()
    ))
}

/// 端口上有没有活着的监听者（200ms 足够确认本机回环）。
fn port_is_live(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// 连上内嵌网页视图的页面 target，返回 (连接, 会话 id)。
///
/// **必须按 URL 匹配 target**：子 webview 的浏览器进程里可能有多个 page
/// target（当前页 + about:blank 残留页），按「第一个」取会取到空页 →
/// 页面级命令全落到错的 target 上（表现为「找不到 iframe」）。
fn connect_page() -> Result<(Cdp, String), String> {
    let (port, ws_path) = devtools_endpoint()?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let targets = cdp.targets()?;
    let current = crate::webview_host::active_webview_url();

    // 1) URL 完全匹配当前页面的 target；
    // 2) 退而求其次：非 about:blank 的 page target；
    // 3) 再退：第一个 page target。
    let page = current
        .as_ref()
        .and_then(|url| {
            targets.iter().find(|t| {
                t.get("type").and_then(|v| v.as_str()) == Some("page")
                    && t.get("url").and_then(|v| v.as_str()) == Some(url.as_str())
            })
        })
        .or_else(|| {
            targets.iter().find(|t| {
                t.get("type").and_then(|v| v.as_str()) == Some("page")
                    && !t.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank").is_empty()
                    && t.get("url").and_then(|v| v.as_str()) != Some("about:blank")
            })
        })
        .or_else(|| {
            targets
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        })
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

/// **按可见文字点击**，并**核对页面是否真的变了**。
///
/// 为什么要有这个原语（2026-10-21 实测踩坑）：agent 用坐标点超星目录里的
/// 「1.2 信息的属性」时，**点击派发了但页面没跳**（点到的是标题 span 而不是
/// 可点的祖先，或点到被遮挡的元素）。agent 如果不核对，就会以为点成功了 →
/// 接着在错误的章节上干活，**而且不会自己发现**。
///
/// 所以这个工具自带闭环：
/// 1. 按文字找候选（取**最深的**那个，通常才是真正可点的）；
/// 2. 点它（先 `el.click()`，没变化再补一次真实鼠标事件序列）；
/// 3. **比对点击前后的 URL + 正文指纹**，如实回报变了没有。
///
/// 返回 `{ clicked, changed, url, before, after }`——`changed: false` 就是
/// 「没点上」，调用方据此换一种点法，而不是闷头往下走。
pub fn click_text(text: &str, frame: Option<&str>) -> Result<Value, String> {
    let (mut cdp, sid) = connect_page()?;
    let needle = serde_json::to_string(text).unwrap_or_default();
    let in_frame = frame.map(str::trim).filter(|f| !f.is_empty());

    let eval_in = |cdp: &mut Cdp, js: &str| -> Result<String, String> {
        match in_frame {
            Some(needle_frame) => cdp.evaluate_in_frame(&sid, needle_frame, js),
            None => evaluate(cdp, &sid, js),
        }
    };

    // 1) 找候选：文字匹配、可见、取**最深的**（a/button 优先、子元素少优先）。
    let find_js = format!(
        r#"(() => {{
          const want = {needle};
          const all = Array.from(document.querySelectorAll('a,button,li,span,div,td'));
          const hit = all.filter(e => {{
            const t = (e.textContent || '').trim();
            if (t !== want && !t.includes(want)) return false;
            const r = e.getBoundingClientRect();
            if (r.width < 2 || r.height < 2) return false;
            const st = getComputedStyle(e);
            return st.visibility !== 'hidden' && st.display !== 'none';
          }});
          if (!hit.length) return JSON.stringify({{ ok: false, why: 'no-match' }});
          const score = (e) => (e.tagName === 'A' || e.tagName === 'BUTTON' ? 0 : 1) * 1000 + e.querySelectorAll('*').length;
          hit.sort((a, b) => score(a) - score(b));
          const el = hit[0];
          const r = el.getBoundingClientRect();
          return JSON.stringify({{ ok: true, tag: el.tagName, cls: String(el.className || ''), x: r.left + r.width/2, y: r.top + r.height/2, count: hit.length }});
        }})()"#
    );
    let found_raw = eval_in(&mut cdp, &find_js)?;
    let found: Value = serde_json::from_str(&found_raw)
        .map_err(|e| format!("候选解析失败：{e}（原始：{found_raw}）"))?;
    if found.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(json!({
            "clicked": false,
            "changed": false,
            "why": found.get("why").and_then(|v| v.as_str()).unwrap_or("no-match"),
            "hint": "页面上没有这个文字；先用 webview_text 看清楚到底有什么文字",
        }));
    }
    let x = found.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = found.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // 2) 点击前先记指纹（URL + 正文前 200 字 + 正文长度）。
    const FINGERPRINT: &str =
        r#"JSON.stringify({u: location.href, t: (document.body ? document.body.innerText : '').slice(0, 200), l: (document.body ? document.body.innerText.length : 0)})"#;
    let before = eval_in(&mut cdp, FINGERPRINT)?;

    // 3) 先用 DOM 点击（合成事件）。
    let click_js = format!(
        r#"(() => {{
          const want = {needle};
          const all = Array.from(document.querySelectorAll('a,button,li,span,div,td'));
          const hit = all.filter(e => {{ const t = (e.textContent||'').trim(); if (t !== want && !t.includes(want)) return false; const r = e.getBoundingClientRect(); return r.width >= 2 && r.height >= 2; }});
          const score = (e) => (e.tagName==='A'||e.tagName==='BUTTON'?0:1)*1000 + e.querySelectorAll('*').length;
          hit.sort((a,b) => score(a)-score(b));
          if (!hit.length) return 'no-match';
          hit[0].click();
          return 'clicked';
        }})()"#
    );
    let dom_click = eval_in(&mut cdp, &click_js)?;

    // 4) 等页面变化（最多 ~4s），变了就停。
    let mut changed = false;
    let mut after = before.clone();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(200));
        after = eval_in(&mut cdp, FINGERPRINT)?;
        if after != before {
            changed = true;
            break;
        }
    }
    // 没变化 → 补一次真实鼠标点击（坐标）。
    if !changed {
        for pressed in [true, false] {
            cdp.call(
                "Input.dispatchMouseEvent",
                json!({"type": if pressed { "mousePressed" } else { "mouseReleased" }, "x": x, "y": y, "button": "left", "clickCount": 1}),
                Some(&sid),
            )?;
        }
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(200));
            after = eval_in(&mut cdp, FINGERPRINT)?;
            if after != before {
                changed = true;
                break;
            }
        }
    }

    Ok(json!({
        "clicked": dom_click != "no-match",
        "changed": changed,
        "url": serde_json::from_str::<Value>(&after)
            .ok()
            .and_then(|v| v.get("u").and_then(|u| u.as_str()).map(|s| s.to_string())),
        "before": serde_json::from_str::<Value>(&before).ok(),
        "after": serde_json::from_str::<Value>(&after).ok(),
        "target": { "tag": found.get("tag"), "cls": found.get("cls"), "x": x, "y": y, "candidates": found.get("count") },
        "hint": if changed { "已跳转" } else { "点了但页面没变：换一种点法（文字更精确，或先滚到可见）" },
    }))
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
///
/// `clip` = (x, y, width, height)，**顶层文档坐标**（CSS 像素，与 CDP 的
/// `Page.captureScreenshot.clip` 语义一致：相对文档左上角，不受滚动影响）。
/// 配合 `scale` 可以只截一小块并放大 —— agent 读页面上的小字（考试题、
/// 表格单元格）时，**整视口截图会被压到 1024 宽，字就糊了**；裁一块 2 倍
/// 放大，既清晰又不浪费上下文。
pub fn screenshot(clip: Option<(f64, f64, f64, f64)>, scale: f64) -> Result<(String, String, String), String> {
    let (mut cdp, sid) = connect_page()?;
    let mut params = json!({"format": "png"});
    match clip {
        Some((x, y, width, height)) => {
            let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
            params["clip"] = json!({
                "x": x, "y": y, "width": width, "height": height, "scale": scale,
            });
        }
        None => {
            params["captureBeyondViewport"] = json!(false);
        }
    }
    let result = cdp.call("Page.captureScreenshot", params, Some(&sid))?;
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

    /// 2026-10-22 的真实故障形态：端口文件里写着 `59280`，那个进程早在
    /// 00:45 就退了，文件留了下来。只检查"文件能不能读"会把这个死端口当成
    /// 有效端点返回，然后连接被拒——报出来的是下游的错，真正的原因看不见。
    #[test]
    fn stale_port_file_is_rejected() {
        let dir = std::env::temp_dir().join("hermix-cdp-stale-port-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 绑一个端口拿到号，立刻释放 → 几乎不可能有人在听。
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        std::fs::write(
            dir.join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/long-gone\n"),
        )
        .unwrap();

        let err = devtools_endpoint_in(&dir, Duration::from_millis(60)).unwrap_err();
        assert!(err.contains("调试端口"), "错误必须点明端口：{err}");
        assert!(
            err.contains("remote-debugging") || err.contains("WebView2"),
            "错误必须指路（环境变量 / 残留进程）：{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端口真的在听 → 才采信这个文件。
    #[test]
    fn live_port_file_is_accepted() {
        let dir = std::env::temp_dir().join("hermix-cdp-live-port-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/alive\n"),
        )
        .unwrap();

        let (got, ws) = devtools_endpoint_in(&dir, Duration::from_millis(500)).unwrap();
        assert_eq!(got, port);
        assert_eq!(ws, "/devtools/browser/alive");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_port_file_reports_clearly() {
        // 没有打开内嵌视图时，端口文件不存在 → 必须给一句能看懂的错误，
        // 而不是 panic 或空串。
        //
        // 用**空的临时目录**：真实 profile 目录是机器全局状态（本机开着内嵌
        // 视图时文件就在），拿它当"缺失"的样本会让测试随机红。
        let dir = std::env::temp_dir().join("hermix-cdp-missing-port-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = devtools_endpoint_in(&dir, Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("调试端口"), "实际错误：{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
