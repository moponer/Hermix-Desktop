//! CDP 客户端（Chrome DevTools Protocol）—— 浏览器通道与内嵌网页视图**共用**。
//!
//! 传输：同步 WebSocket（tungstenite 0.29，已在 Cargo.lock → 零新增下载）。
//! 为什么同步：MCP 服务与 Tauri 命令都是同步/阻塞语境（MCP 的 stdio 主循环是
//! 逐行阻塞读的），套一层 async runtime 只会把「等一个响应」变成「等一个
//! 跨 await 的响应」，没有收益。
//!
//! 两个使用者：
//! - `browser.rs`：连**系统浏览器**（Edge/Chrome，自己拉起来的进程）；
//! - `webview_cdp.rs`：连**查看器内嵌网页视图**（WebView2，开调试端口的那个
//!   浏览器进程）。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::client::{client as ws_client, IntoClientRequest};
use tungstenite::{Message, WebSocket};

/// 单次读写的超时。
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// 一条 CDP 连接。
pub struct Cdp {
    ws: WebSocket<TcpStream>,
    next_id: u64,
    /// `Runtime.executionContextCreated` 事件里收集到的
    /// `frameId → contextId`（跨源 iframe 的执行上下文）。
    contexts: std::collections::HashMap<String, u64>,
}

impl Cdp {
    /// 连到某个 **browser 级**端点（`ws://127.0.0.1:<port><path>`）。
    pub fn connect(port: u16, ws_path: &str) -> Result<Self, String> {
        let addr = SocketAddr::from((IpAddr::V4(Ipv4Addr::LOCALHOST), port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .map_err(|e| format!("连接浏览器调试端口失败：{e}"))?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let request = format!("ws://127.0.0.1:{port}{ws_path}")
            .into_client_request()
            .map_err(|e| format!("调试地址无法解析：{e}"))?;
        let (ws, _resp) = ws_client(request, stream).map_err(|e| format!("浏览器握手失败：{e}"))?;
        Ok(Self {
            ws,
            next_id: 0,
            contexts: std::collections::HashMap::new(),
        })
    }

    /// 发一条命令并等它的响应（跳过事件消息）。
    ///
    /// `session` = 已 attach 的页面会话（`Target.attachToTarget` 返回的
    /// sessionId）；传 None 就是 browser 级命令。
    pub fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session {
            message["sessionId"] = json!(sid);
        }
        self.ws
            .send(Message::Text(message.to_string().into()))
            .map_err(|e| format!("发送 {method} 失败：{e}"))?;

        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                return Err(format!("等待 {method} 响应超时"));
            }
            let text = match self.ws.read() {
                Ok(Message::Text(t)) => t.as_str().to_string(),
                Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).to_string(),
                Ok(Message::Close(_)) => return Err("浏览器连接已关闭".to_string()),
                Ok(_) => continue,
                Err(e) => return Err(format!("读取 {method} 响应失败：{e}")),
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // 顺手收下执行上下文事件（跨源 iframe 的 contextId 就在里面）。
            if value.get("method").and_then(|m| m.as_str()) == Some("Runtime.executionContextCreated") {
                if let (Some(frame_id), Some(context_id)) = (
                    value
                        .get("params")
                        .and_then(|p| p.get("context"))
                        .and_then(|c| c.get("auxData"))
                        .and_then(|a| a.get("frameId"))
                        .and_then(|v| v.as_str()),
                    value
                        .get("params")
                        .and_then(|p| p.get("context"))
                        .and_then(|c| c.get("id"))
                        .and_then(|v| v.as_u64()),
                ) {
                    self.contexts.insert(frame_id.to_string(), context_id);
                }
            }
            if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue; // 事件，或别的命令的响应
            }
            if let Some(err) = value.get("error") {
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("未知错误");
                return Err(format!("浏览器报错：{message}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// 挂到某个页面 target 上，返回 sessionId（target 级隔离）。
    pub fn attach(&mut self, target_id: &str) -> Result<String, String> {
        let result = self.call(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
        )?;
        result
            .get("sessionId")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "挂接页面失败：浏览器没有返回 sessionId".to_string())
    }

    /// 列出全部页面 target（`Target.getTargets`）。
    pub fn targets(&mut self) -> Result<Vec<Value>, String> {
        let result = self.call("Target.getTargets", json!({}), None)?;
        Ok(result
            .get("targetInfos")
            .and_then(|t| t.as_array())
            .map(|a| a.to_vec())
            .unwrap_or_default())
    }

    /// 打开 `Runtime` 域并收下各 iframe 的执行上下文。
    ///
    /// **必须带页面会话**：`Runtime` / `Page` 是**页面级**域，发给 browser 级
    /// 端点会报 "'Runtime.enable' wasn't found"。
    pub fn enable_runtime(&mut self, page_session: &str) -> Result<(), String> {
        self.call("Runtime.enable", json!({}), Some(page_session))
            .map(|_| ())
    }

    /// 在**跨源 iframe** 里求值。
    ///
    /// 顶层 `Runtime.evaluate` 受同源策略限制，够不到别的域的 iframe 里的
    /// 元素（超星的视频播放器就在 `/ananas/modules/video/index.html` 这个
    /// 跨源 iframe 里）。CDP 的做法是：先开 `Runtime` 域拿到每个 frame 的
    /// `contextId`，再用 `Runtime.evaluate{contextId}` 在那个上下文里执行。
    ///
    /// `frame_needle` = 目标 iframe 的 URL 片段。
    pub fn evaluate_in_frame(
        &mut self,
        page_session: &str,
        frame_needle: &str,
        expression: &str,
    ) -> Result<String, String> {
        self.enable_runtime(page_session)?;
        let tree = self.call("Page.getFrameTree", json!({}), Some(page_session))?;
        let mut frame_id: Option<String> = None;
        let mut stack: Vec<Value> = Vec::new();
        if let Some(root) = tree.get("frameTree") {
            stack.push(root.clone());
        }
        while let Some(node) = stack.pop() {
            if let Some(url) = node.get("url").and_then(|v| v.as_str()) {
                if url.contains(frame_needle) {
                    frame_id = node.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                    break;
                }
            }
            if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
                for child in children {
                    stack.push(child.clone());
                }
            }
        }
        let frame_id = frame_id.ok_or_else(|| {
            format!("页面里没有 URL 含「{frame_needle}」的 iframe")
        })?;
        let context_id = self
            .contexts
            .get(&frame_id)
            .copied()
            .ok_or_else(|| format!("找不到该 iframe 的执行上下文（{frame_needle}）"))?;
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
                "contextId": context_id,
            }),
            Some(page_session),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("脚本执行异常");
            return Err(format!("脚本异常：{text}"));
        }
        Ok(match result.get("result").and_then(|r| r.get("value")) {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        })
    }
}

/// 在页面里求值一个 JS 表达式，返回按值的结果。
pub fn evaluate(cdp: &mut Cdp, session: &str, expression: &str) -> Result<String, String> {
    let result = cdp.call(
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
        Some(session),
    )?;
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("脚本执行异常");
        return Err(format!("脚本异常：{text}"));
    }
    Ok(match result.get("result").and_then(|r| r.get("value")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    })
}

/// 截图的落盘 + 缩略图（给模型看的那份）。
///
/// 原始 PNG 直接塞进模型上下文就是上下文炸弹 → 缩到最长边
/// `SHOT_MAX_WIDTH` 再转 JPEG q72；原图落盘，用户能直接打开看。
pub fn save_and_thumbnail(png: &[u8], name: &str) -> Result<(String, String, String), String> {
    let dir = hermix_config::paths::hermix_home().join("browser-shots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建截图目录失败：{e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{name}-{stamp}.png"));
    std::fs::write(&path, png).map_err(|e| format!("写入截图失败：{e}"))?;

    let (thumb_b64, mime) = match thumbnail_jpeg(png) {
        Ok(v) => v,
        Err(_) => (crate::commands::base64_encode(png), "image/png".to_string()),
    };
    Ok((path.to_string_lossy().to_string(), thumb_b64, mime))
}

/// 缩略图：最长边缩到 `SHOT_MAX_WIDTH`，转 JPEG。
fn thumbnail_jpeg(png: &[u8]) -> Result<(String, String), String> {
    let img = image::load_from_memory(png).map_err(|e| format!("截图解码失败：{e}"))?;
    let resized = img.thumbnail(SHOT_MAX_WIDTH, SHOT_MAX_WIDTH);
    let rgb = resized.to_rgb8();
    let mut buf: Vec<u8> = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 72);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("截图压缩失败：{e}"))?;
    Ok((crate::commands::base64_encode(&buf), "image/jpeg".to_string()))
}

/// 截图缩放后的最大宽度（给模型看的图不能太大）。
const SHOT_MAX_WIDTH: u32 = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_rejects_a_dead_port() {
        // 1 端口几乎肯定没人监听 → 必须报「连接失败」而不是 panic。
        // （不用 unwrap_err：Cdp 不实现 Debug，会逼我们为一个断言加派生。）
        let err = Cdp::connect(1, "/devtools/browser/x").err().unwrap_or_default();
        assert!(err.contains("连接浏览器调试端口失败"), "实际错误：{err}");
    }
}
