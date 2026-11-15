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

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cdp::{evaluate, Cdp};

/// 等调试端口就绪的上限。
const PORT_TIMEOUT: Duration = Duration::from_secs(10);

/// 读 `DevToolsActivePort` → (端口, ws 路径)。端口没就绪就等一会儿。
///
/// **路径要试两处**：WebView2 把用户数据放在 UDF 的 `EBWebView` 子目录里
/// （`DevToolsActivePort` 在那里面），而系统浏览器（Edge/Chrome）是把文件
/// 直接写在 UDF 根下。两处都找，才不会因为引擎不同而读不到端口。
fn devtools_endpoint() -> Result<(u16, String), String> {
    devtools_endpoint_in(&crate::webview_host::user_data_folder(), PORT_TIMEOUT)
}

/// 两处候选端口文件（顺序：UDF 根 → `EBWebView` 子目录）。
fn endpoint_candidates(base: &Path) -> [PathBuf; 2] {
    [
        base.join("DevToolsActivePort"),
        base.join("EBWebView").join("DevToolsActivePort"),
    ]
}

/// 清掉上一轮进程留下的 `DevToolsActivePort`。
///
/// WebView2 只在**本次进程**创建了网页视图之后才写这个文件；崩溃或正常退出都会
/// 把上一轮的端口留在原处，读到它就等于连一个没人听的端口（10061）。
///
/// **只允许持有单实例互斥的进程调用**：第二个实例若也清，会把活实例正在用的
/// 端口文件删掉（查看器的 `--mcp` 模式在上游已提前返回，不会走到这里）。
pub fn discard_stale_endpoint() {
    for file in endpoint_candidates(&crate::webview_host::user_data_folder()) {
        if file.exists() {
            let _ = std::fs::remove_file(&file);
        }
    }
}

/// 端口是否真的有进程在听。**文件存在 ≠ 端口活着**。
fn port_is_live(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// 解析端口文件的前两行 → (端口, ws 路径)。
fn read_endpoint(file: &Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines();
    let port = lines.next()?.trim().parse::<u16>().ok()?;
    let path = lines.next()?.trim();
    (!path.is_empty()).then(|| (port, path.to_string()))
}

/// 同上，但**基目录与超时都显式传入** —— 测试要用一个空的临时目录，
/// 否则本机只要开着内嵌视图，真实 profile 里就有 `DevToolsActivePort`，
/// 测试的前提（"没开视图"）根本不成立。
fn devtools_endpoint_in(base: &Path, timeout: Duration) -> Result<(u16, String), String> {
    let candidates = endpoint_candidates(base);
    let deadline = Instant::now() + timeout;
    loop {
        for file in &candidates {
            let Some((port, path)) = read_endpoint(file) else {
                continue;
            };
            if port_is_live(port) {
                return Ok((port, path));
            }
            // 连不上 = 上一轮的残留（WebView2 只在环境就绪后才写这个文件）。
            // 删掉它，否则每一轮都会立刻采信同一个死端口、立刻失败。
            let _ = std::fs::remove_file(file);
        }
        if Instant::now() >= deadline {
            return Err(endpoint_error(base, &candidates));
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// 端口不可用时的错误体：把判断依据一起给出来（否则调用方只能猜）。
fn endpoint_error(base: &Path, candidates: &[PathBuf]) -> String {
    let present: Vec<String> = candidates
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect();
    let view_exists = crate::webview_host::active_webview_url().is_some();
    let hint = if view_exists {
        "视图在但端口没就绪：稍后重试一次；持续失败就重启查看器（不要删 profile，里面是登录态）"
    } else {
        "先用 web_open 打开一个页面（这一步才会创建视图并写端口）"
    };
    format!(
        "内嵌网页视图的调试端口不可用 | code=no_live_endpoint | udf={} | port_file={} | \
         port_reachable=false | view_exists={view_exists} | hint={hint}",
        base.display(),
        if present.is_empty() {
            "none".to_string()
        } else {
            present.join(";")
        }
    )
}

/// 连上内嵌网页视图的页面 target，返回 (连接, 会话 id)。
///
/// **必须按 URL 匹配 target**：子 webview 的浏览器进程里可能有多个 page
/// target（当前页 + about:blank 残留页），按「第一个」取会取到空页 →
/// 页面级命令全落到错的 target 上（表现为「找不到 iframe」）。
fn connect_page() -> Result<(Cdp, String), String> {
    let (port, ws_path) = devtools_endpoint()?;
    // 端口刚才探活过还是失败 → 视图进程在两次调用之间退出了（或环境正在重建）。
    // 把这一点写进错误里，否则上层只能看到"连接调试端口失败"。
    let mut cdp = Cdp::connect(port, &ws_path).map_err(|e| {
        format!(
            "{e} | code=connect_failed | port={port} | udf={} | hint=视图进程刚退出：\
             重新 web_open 一次（这会重建视图并写新的端口）",
            crate::webview_host::user_data_folder().display()
        )
    })?;
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

/// 导航后回读到的**实际落点**。
struct Landing {
    url: String,
    title: String,
    text: String,
    /// `document.readyState !== "loading"`（页面不再加载中）。
    ready: bool,
}

/// 读页面落点（URL / 标题 / 正文前 240 字 / 是否已加载完）。
fn read_landing(cdp: &mut Cdp, sid: &str) -> Landing {
    let js = "JSON.stringify({url: location.href, title: document.title, \
              text: ((document.body && document.body.innerText) || '').slice(0, 240), \
              ready: document.readyState !== 'loading'})";
    let raw = evaluate(cdp, sid, js).unwrap_or_default();
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) => Landing {
            url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            title: v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            ready: v.get("ready").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        Err(_) => Landing {
            url: String::new(),
            title: String::new(),
            text: String::new(),
            ready: false,
        },
    }
}

/// 等页面稳定后回读落点。
///
/// 为什么必须等：`Page.navigate` 只是把请求发出去。服务端 302 到登录页时，
/// 立刻读 `location.href` 拿到的还是**上一个**地址 —— 看起来像导航成功。
/// 先给服务端跳转留 600ms，再等 `readyState` 离开 loading（上限 7s）。
fn wait_landing(cdp: &mut Cdp, sid: &str) -> Landing {
    std::thread::sleep(Duration::from_millis(600));
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let landing = read_landing(cdp, sid);
        if landing.url.is_empty() || landing.ready || Instant::now() >= deadline {
            return landing;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// 落点是否像「登录页 / 提示页」而不是目标页；返回命中依据。
///
/// 这是「工具必须如实回报」的一部分（2026-11-05）：此前 `navigate` 直接把
/// **请求的 URL** 当结果返回，页面被重定向到登录页时 agent 收到的仍是它请求
/// 的那个地址 → 每一轮都「成功」，于是在同一入口上连续空转 18 轮。
pub(crate) fn gate_reason(url: &str, title: &str, text: &str) -> Option<String> {
    let u = url.to_ascii_lowercase();
    for marker in ["passport", "/login", "signin", "/sso", "cas/", "authserver"] {
        if u.contains(marker) {
            return Some(format!("URL 含 `{marker}`"));
        }
    }
    for marker in [
        "请重新登录",
        "重新登录",
        "登录已失效",
        "登录超时",
        "请先登录",
        "账号登录",
        "扫码登录",
        "请输入密码",
        "温馨提示",
        "id被禁用",
    ] {
        if title.contains(marker) || text.contains(marker) {
            return Some(format!("页面文案含「{marker}」"));
        }
    }
    None
}

/// 把落点描述成给模型看的文本（含「未到达目标页」的明确结论）。
fn describe_landing(requested: &str, landing: &Landing) -> String {
    let landed = if landing.url.is_empty() {
        requested
    } else {
        landing.url.as_str()
    };
    let mut out = format!("已导航到 {landed}");
    if !landing.title.is_empty() {
        out.push_str(&format!("\n页面标题：{}", landing.title));
    }
    if let Some(why) = gate_reason(&landing.url, &landing.title, &landing.text) {
        out.push_str(&format!(
            "\n⚠ 未到达目标页：落点像登录/提示页（依据：{why}）。请求的是 {requested}。\
             \n这不等于导航成功——多半是登录态失效。不要在同一个入口上反复重试；\
             先解决登录（让用户在这块预览区里手动登录，或用 browser_open + 持久 profile），再回来继续。"
        ));
        if !landing.text.is_empty() {
            let snippet: String = landing.text.chars().take(120).collect();
            out.push_str(&format!("\n页面正文开头：{}", snippet.replace('\n', " ")));
        }
    }
    out
}

/// 在内嵌网页视图里跳转，并**回报实际落点**（不是假定落点）。
///
/// 返回 `(给模型看的报告, 世界状态签名)`。签名不塞进正文让模型去解析 ——
/// 它走 MCP 的 `_meta` 扩展位（结构化），agent 的零推进账本只认那个字段。
pub fn navigate(url: &str) -> Result<(String, String), String> {
    let target = crate::webview_host::normalize_url(url)?;
    let (mut cdp, sid) = connect_page()?;
    cdp.call("Page.navigate", json!({ "url": target.as_str() }), Some(&sid))?;
    let landing = wait_landing(&mut cdp, &sid);
    let landed = if landing.url.is_empty() {
        target.as_str()
    } else {
        landing.url.as_str()
    };
    Ok((
        describe_landing(target.as_str(), &landing),
        format!("url={landed} | title={}", landing.title),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 错误里要带足判断依据：哪个目录、端口文件在不在、视图在不在。
        assert!(err.contains("code=no_live_endpoint"), "实际错误：{err}");
        assert!(err.contains("port_file=none"), "实际错误：{err}");
        assert!(err.contains("view_exists="), "实际错误：{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_port_file_is_discarded_instead_of_connected() {
        // 崩溃后 `DevToolsActivePort` 留在盘上，端口没人听。旧实现会立刻采信它
        // → 上层拿到 10061，把"视图没开"误诊成"连接坏了"。
        let dir = std::env::temp_dir().join("hermix-cdp-stale-port-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("EBWebView")).unwrap();
        // 1 端口几乎肯定没人监听。
        std::fs::write(dir.join("EBWebView").join("DevToolsActivePort"), "1\n/devtools/browser/x").unwrap();

        let err = devtools_endpoint_in(&dir, Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("code=no_live_endpoint"), "实际错误：{err}");
        assert!(
            !dir.join("EBWebView").join("DevToolsActivePort").exists(),
            "死端口文件必须被删掉，否则每一轮都会立刻采信它"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_port_file_is_accepted() {
        // 反向用例：端口真的有进程在听 → 必须采信，且返回文件里的路径。
        let dir = std::env::temp_dir().join("hermix-cdp-live-port-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("EBWebView")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.join("EBWebView").join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/abc"),
        )
        .unwrap();

        let got = devtools_endpoint_in(&dir, Duration::from_millis(50)).unwrap();
        assert_eq!(got.0, port);
        assert_eq!(got.1, "/devtools/browser/abc");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn login_page_is_recognised_from_url_or_copy() {
        assert!(gate_reason("https://passport.chaoxing.com/x", "", "").is_some());
        assert!(gate_reason("https://a.example.com/login?next=/x", "", "").is_some());
        assert!(gate_reason("https://a.example.com/base", "温馨提示", "").is_some());
        assert!(gate_reason("", "", "您的登录已失效，请重新登录").is_some());
        // 正常页不得误报：普通正文提到「登录」不算。
        assert!(gate_reason("https://a.example.com/base", "课程列表", "退出登录").is_none());
    }

    #[test]
    fn navigation_report_names_the_landing_and_warns_on_gate() {
        let gated = Landing {
            url: "https://i.chaoxing.com/base?ws=1".into(),
            title: "温馨提示".into(),
            text: "请重新登录".into(),
            ready: true,
        };
        let report = describe_landing("https://mooc1.chaoxing.com/mooc-ans/x", &gated);
        assert!(report.contains("i.chaoxing.com"), "必须回报实际落点");
        assert!(report.contains("未到达目标页"), "必须明确否掉「成功」");
        assert!(report.contains("mooc1.chaoxing.com"), "要带上请求的地址作对照");

        let ok = Landing {
            url: "https://mooc1.chaoxing.com/mooc-ans/x".into(),
            title: "课程".into(),
            text: "第一章".into(),
            ready: true,
        };
        let report = describe_landing("https://mooc1.chaoxing.com/mooc-ans/x", &ok);
        assert!(!report.contains("未到达目标页"));
    }
}
