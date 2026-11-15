//! 企业微信「智能机器人」WS 通道。
//!
//! 协议对着官方 SDK（`@wecom/aibot-node-sdk`）抄：一条
//! `wss://openws.work.weixin.qq.com` 长连接，建连后先发
//! `aibot_subscribe` 认证，之后 30s 一次 `ping`，收
//! `aibot_msg_callback`，回 `aibot_respond_msg`。本机不需要公网地址。
//!
//! 和 QQ 的关键差异：企业微信的回复必须带**入站帧的 `req_id`**，所以每个
//! 用户最近一次的 `req_id` 得自己存着——这就是 cc-haha `ChatPort` 刻意不把
//! 回复上下文放进接口、留给平台侧的原因。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_tungstenite::tungstenite::Message;

use super::port::{ImPort, ImStream, Inbound};

// ---------------------------------------------------------------------------
// 协议常量
// ---------------------------------------------------------------------------

const WS_URL: &str = "wss://openws.work.weixin.qq.com";

const CMD_SUBSCRIBE: &str = "aibot_subscribe";
const CMD_PING: &str = "ping";
const CMD_RESPOND_MSG: &str = "aibot_respond_msg";
const CMD_SEND_MSG: &str = "aibot_send_msg";
const CMD_MSG_CALLBACK: &str = "aibot_msg_callback";

const HEARTBEAT_MS: u64 = 30_000;
/// 连续 2 次 ping 没回执就认为连接已死（与官方 SDK 一致）。
const MAX_MISSED_PONG: u32 = 2;

const RECONNECT_DELAYS_MS: [u64; 6] = [1000, 2000, 5000, 10000, 30000, 30000];

/// 单条主动消息的字节上限。
const TEXT_BYTE_LIMIT: usize = 4000;
/// 平台单帧上限 20480 字节，留出协议自身开销后的安全线。
const STREAM_BYTE_LIMIT: usize = 19_000;

/// 流式帧的最小间隔。气泡是原地刷新的，刷得快成本低。
const STREAM_THROTTLE_MS: u64 = 500;

fn utf8_len(s: &str) -> usize {
    s.len()
}

/// 按 UTF-8 字节切，不能按字符切——中文一字符 3 字节。
pub fn split_by_bytes(text: &str, limit: usize) -> Vec<String> {
    if utf8_len(text) <= limit {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if utf8_len(&cur) + ch.len_utf8() > limit {
            if cur.is_empty() {
                // 单个字符就超限（不可能发生，limit 远大于 4）：硬塞进去。
                out.push(ch.to_string());
                continue;
            }
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn new_req_id(prefix: &str) -> String {
    let ts = chrono::Utc::now().timestamp_millis();
    let rand: u64 = rand::random();
    format!("{prefix}_{ts}_{rand:x}")
}

// ---------------------------------------------------------------------------
// 入站
// ---------------------------------------------------------------------------

/// 从 `aibot_msg_callback` 的 `body` 抽出可路由的私聊消息。
///
/// 只处理单聊：配对授权的是一个人，群消息等于把这份授权扩散给整个群。
/// 六种消息形态（text/voice/mixed/file/image/video）的文字都落在不同
/// 位置，这里一次性摊平。
pub fn parse_inbound(body: &Value, req_id: &str) -> Option<Inbound> {
    let user = body
        .get("from")
        .and_then(|f| f.get("userid"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if user.is_empty() {
        return None;
    }
    if body.get("chattype").and_then(Value::as_str) == Some("group") {
        return None;
    }

    let text = extract_text(body);
    let quoted = body
        .get("quote")
        .map(extract_text)
        .unwrap_or_default();
    // 被引用的原文作为上下文带进去——引用回复在 IM 里是常见的提问方式。
    let text = if quoted.is_empty() {
        text
    } else {
        let quoted = quoted.replace('\n', "\n> ");
        format!("> {quoted}\n\n{text}").trim().to_string()
    };

    let msgid = body.get("msgid").and_then(Value::as_str).unwrap_or("").trim();
    let created = body
        .get("create_time")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let dedup_key = if msgid.is_empty() {
        format!(
            "{user}:{created}:{}",
            text.chars().take(16).collect::<String>()
        )
    } else {
        msgid.to_string()
    };

    Some(Inbound {
        user,
        // 企业微信的 WS 通道不给昵称，只给 userid。名字缺失时留空，
        // 别编一个「企微用户」去覆盖真实标识。
        display_name: String::new(),
        text,
        dedup_key,
        anchor: req_id.to_string(),
    })
}

/// 摊平各种消息形态里的正文。语音由平台服务端转写，所以也是纯文本。
fn extract_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |v: Option<&str>| {
        if let Some(s) = v.map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(s.to_string());
        }
    };
    push(body.get("text").and_then(|t| t.get("content")).and_then(Value::as_str));
    push(body.get("voice").and_then(|v| v.get("content")).and_then(Value::as_str));
    if let Some(items) = body.get("mixed").and_then(|m| m.get("msg_item")).and_then(Value::as_array)
    {
        for item in items {
            if item.get("msgtype").and_then(Value::as_str) == Some("text") {
                push(item.get("text").and_then(|t| t.get("content")).and_then(Value::as_str));
            }
        }
    }
    parts.join("\n").trim().to_string()
}

#[derive(Debug, Deserialize)]
struct WsFrame {
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    body: Value,
    #[serde(default)]
    errcode: Option<i64>,
}

impl WsFrame {
    fn req_id(&self) -> String {
        self.headers
            .as_ref()
            .and_then(|h| h.get("req_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// 端口
// ---------------------------------------------------------------------------

/// 企业微信端口。回包要挂在入站 `req_id` 上，所以这里存每个用户最近一次
/// 的锚点。
pub struct WecomPort {
    bot_id: String,
    secret: String,
    /// `userid -> 最近一次入站的 req_id`
    anchors: Mutex<HashMap<String, String>>,
    /// 发送通道。网关联上后才有；没连上时发送会失败。
    tx: mpsc::UnboundedSender<String>,
}

impl WecomPort {
    pub fn new(bot_id: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            bot_id: bot_id.into(),
            secret: secret.into(),
            anchors: Mutex::new(HashMap::new()),
            tx: mpsc::unbounded_channel().0,
        }
    }

    /// 网关循环拿到同一个发送端，两边共用一条 socket。
    pub fn attach_sender(&mut self, tx: mpsc::UnboundedSender<String>) {
        self.tx = tx;
    }

    async fn send_frame(&self, frame: Value) -> Result<(), String> {
        self.tx
            .send(frame.to_string())
            .map_err(|_| "企业微信网关未连接".to_string())
    }

    async fn last_anchor(&self, user: &str) -> String {
        self.anchors
            .lock()
            .await
            .get(user)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ImPort for WecomPort {
    fn platform(&self) -> &'static str {
        "wecom"
    }

    async fn send_text(&self, user: &str, content: &str, anchor: &str) -> Result<(), String> {
        for chunk in split_by_bytes(content, TEXT_BYTE_LIMIT) {
            if chunk.trim().is_empty() {
                continue;
            }
            let body = json!({ "msgtype": "markdown", "markdown": { "content": chunk } });
            // 有锚点就回在用户那条消息下面，没有就走主动消息。
            let (cmd, req_id, mut full) = if anchor.is_empty() {
                (CMD_SEND_MSG, new_req_id("send"), body)
            } else {
                (CMD_RESPOND_MSG, anchor.to_string(), body)
            };
            if cmd == CMD_SEND_MSG {
                full["chatid"] = json!(user);
            }
            self.send_frame(json!({
                "cmd": cmd,
                "headers": { "req_id": req_id },
                "body": full,
            }))
            .await?;
        }
        Ok(())
    }

    fn start_stream(&self, user: &str, anchor: &str) -> Box<dyn ImStream> {
        Box::new(WecomStream {
            user: user.to_string(),
            req_id: anchor.to_string(),
            stream_id: new_req_id("stream").replace(['_', '-'], ""),
            header: String::new(),
            shown: String::new(),
            overflow: String::new(),
            last_flush: None,
            closed: false,
            tx: self.tx.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// 流式回复
// ---------------------------------------------------------------------------

/// 一轮助手输出渲染成一条企业微信流式消息（同一个 `stream_id` 原地刷新）。
///
/// 两个约束决定了它的形状：回复必须挂在入站帧的 `req_id` 上；单个气泡有
/// 字节上限，超出的部分改成普通主动消息继续发，而不是被静默截断。
pub struct WecomStream {
    user: String,
    req_id: String,
    stream_id: String,
    /// 进度行（工具调用轨迹），显示在正文上方。
    header: String,
    shown: String,
    overflow: String,
    last_flush: Option<std::time::Instant>,
    closed: bool,
    tx: mpsc::UnboundedSender<String>,
}

impl WecomStream {
    /// 进度行 + 正文。
    fn plain(&self) -> String {
        if self.header.is_empty() {
            self.shown.clone()
        } else if self.shown.is_empty() {
            self.header.clone()
        } else {
            format!("{}

{}", self.header, self.shown)
        }
    }

    async fn push_frame(&self, content: &str, finish: bool) -> Result<(), String> {
        self.tx
            .send(
                json!({
                    "cmd": CMD_RESPOND_MSG,
                    "headers": { "req_id": self.req_id },
                    "body": {
                        "msgtype": "stream",
                        "stream": { "id": self.stream_id, "content": content, "finish": finish }
                    }
                })
                .to_string(),
            )
            .map_err(|_| "企业微信网关未连接".to_string())
    }

    async fn push_proactive(&self, content: &str) {
        if content.trim().is_empty() {
            return;
        }
        for chunk in split_by_bytes(content, TEXT_BYTE_LIMIT) {
            let _ = self
                .tx
                .send(
                    json!({
                        "cmd": CMD_SEND_MSG,
                        "headers": { "req_id": new_req_id("send") },
                        "body": {
                            "chatid": self.user,
                            "msgtype": "markdown",
                            "markdown": { "content": chunk }
                        }
                    })
                    .to_string(),
                );
        }
    }

    async fn append_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.closed {
            self.overflow.push_str(delta);
            return;
        }
        let next = format!("{}{delta}", self.plain());
        if utf8_len(&next) > STREAM_BYTE_LIMIT {
            // 把气泡定稿在最后一个装得下的帧，剩余部分走主动消息。
            if let Err(e) = self.push_frame(&self.shown, true).await {
                tracing::warn!(error = %e, "企业微信流式定稿失败，改用主动消息");
            }
            self.closed = true;
            self.overflow.push_str(delta);
            return;
        }
        self.shown = next;

        let due = self
            .last_flush
            .map(|t| t.elapsed() >= Duration::from_millis(STREAM_THROTTLE_MS))
            .unwrap_or(true);
        if !due {
            return;
        }
        let snapshot = self.plain();
        if let Err(e) = self.push_frame(&snapshot, false).await {
            tracing::warn!(error = %e, "企业微信流式刷新失败，本轮剩余部分改用主动消息");
            self.closed = true;
            self.overflow.push_str(&snapshot);
        } else {
            self.last_flush = Some(std::time::Instant::now());
        }
    }

    /// 工具开跑：清掉已流出的叙述（同 `QqStream::clear_text`）。
    async fn clear_text(&mut self) {
        if self.shown.is_empty() {
            return;
        }
        self.shown = String::new();
        if self.closed {
            return;
        }
        let snapshot = self.plain();
        if let Err(e) = self.push_frame(&snapshot, false).await {
            tracing::warn!(error = %e, "企业微信清屏帧失败");
        } else {
            self.last_flush = Some(std::time::Instant::now());
        }
    }

    async fn complete(&mut self) {
        if !self.closed {
            let snapshot = self.plain();
            if let Err(e) = self.push_frame(&snapshot, true).await {
                tracing::warn!(error = %e, "企业微信流式定稿失败");
                self.overflow.push_str(&snapshot);
            }
            self.closed = true;
        }
        let tail = std::mem::take(&mut self.overflow);
        self.push_proactive(tail.trim()).await;
    }
}

#[async_trait]
impl ImStream for WecomStream {
    async fn append(&mut self, delta: &str) {
        self.append_delta(delta).await;
    }

    async fn set_header(&mut self, header: &str) {
        if self.header == header {
            return;
        }
        self.header = header.to_string();
        if self.closed {
            return;
        }
        // 同 QQ：工具执行期间没有正文 delta，进度行必须主动刷帧才有
        // "还活着"信号，否则企微端整段工具期毫无动静。
        let due = self
            .last_flush
            .map(|t| t.elapsed() >= Duration::from_millis(STREAM_THROTTLE_MS))
            .unwrap_or(true);
        if !due {
            return;
        }
        let snapshot = self.plain();
        // 这里失败不降级：进度行丢了不打紧，下一次正文 flush 自会重试
        // 并按既有路径处理连续失败。
        if let Err(e) = self.push_frame(&snapshot, false).await {
            tracing::warn!(error = %e, "企业微信进度行刷新失败");
        } else {
            self.last_flush = Some(std::time::Instant::now());
        }
    }

    async fn finish(&mut self) {
        self.complete().await;
    }
}

// ---------------------------------------------------------------------------
// 网关
// ---------------------------------------------------------------------------

/// 网关主循环。建连 → `aibot_subscribe` 认证 → 30s `ping` → 收帧 → 回执。
///
/// 整条 socket 由**一个** task 独占：认证、心跳、读帧、停机四条线在一个
/// `select!` 里，谁也不阻塞谁。
pub async fn run_gateway(
    port: Arc<WecomPort>,
    events: mpsc::Sender<Inbound>,
    mut shutdown: watch::Receiver<bool>,
    mut commands: mpsc::UnboundedReceiver<String>,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    let mut attempt = 0usize;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let result = connect_and_pump(&port, &events, &mut shutdown, &mut commands, connected.clone()).await;
        // 出 pumps 即断线，统一复位——与 QQ 侧同一套状态语义。
        connected.store(false, std::sync::atomic::Ordering::Relaxed);
        match result {
            Ok(()) => {
                if let Ok(mut g) = last_error.lock() {
                    *g = None;
                }
                attempt = 0;
            }
            Err(e) => {
                tracing::warn!(error = %e, "企业微信网关中断，准备重连");
                if let Ok(mut g) = last_error.lock() {
                    *g = Some(e);
                }
                attempt = (attempt + 1).min(RECONNECT_DELAYS_MS.len() - 1);
            }
        }
        if *shutdown.borrow() {
            return;
        }
        let delay = Duration::from_millis(RECONNECT_DELAYS_MS[attempt]);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => return,
        }
    }
}

async fn connect_and_pump(
    port: &Arc<WecomPort>,
    events: &mpsc::Sender<Inbound>,
    shutdown: &mut watch::Receiver<bool>,
    commands: &mut mpsc::UnboundedReceiver<String>,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .map_err(|e| format!("连接企业微信网关失败: {e}"))?;
    tracing::info!(bot_id = %port.bot_id, "企业微信网关已连接");

    let auth = json!({
        "cmd": CMD_SUBSCRIBE,
        "headers": { "req_id": new_req_id("sub") },
        "body": { "bot_id": port.bot_id, "secret": port.secret },
    });
    if let Err(e) = ws.send(Message::Text(auth.to_string().into())).await {
        return Err(format!("发送认证帧失败: {e}"));
    }

    let mut heartbeat = tokio::time::interval(Duration::from_millis(HEARTBEAT_MS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut missed_pong = 0u32;
    let mut authed = false;

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                let _ = ws.close(None).await;
                return Ok(());
            }
            _ = heartbeat.tick() => {
                if missed_pong >= MAX_MISSED_PONG {
                    tracing::warn!("连续 {MAX_MISSED_PONG} 次 ping 无回执，判定连接已死");
                    return Err("心跳超时".into());
                }
                missed_pong += 1;
                let ping = json!({ "cmd": CMD_PING, "headers": { "req_id": new_req_id("ping") } });
                if ws.send(Message::Text(ping.to_string().into())).await.is_err() {
                    return Err("发送心跳失败".into());
                }
            }
            Some(out) = commands.recv() => {
                if ws.send(Message::Text(out.into())).await.is_err() {
                    return Err("发送业务帧失败".into());
                }
            }
            next = ws.next() => {
                let Some(next) = next else { return Ok(()) };
                let text = match next {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Ping(p)) => { let _ = ws.send(Message::Pong(p)).await; continue; }
                    Ok(Message::Close(_)) => return Ok(()),
                    Ok(_) => continue,
                    Err(e) => return Err(format!("读帧失败: {e}")),
                };
                let frame: WsFrame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => { tracing::warn!(error = %e, "企业微信帧解析失败，忽略"); continue; }
                };
                // 认证成功前只认订阅回执，其它一律忽略。
                if !authed {
                    if frame.cmd.is_none() && frame.errcode == Some(0) {
                        authed = true;
                        connected.store(true, std::sync::atomic::Ordering::Relaxed);
                        tracing::info!("企业微信智能机器人已认证");
                        continue;
                    }
                    if frame.errcode.is_some_and(|c| c != 0) {
                        return Err(format!(
                            "企业微信认证失败 errcode={} errmsg={}",
                            frame.errcode.unwrap_or_default(),
                            frame.headers.as_ref().and_then(|h| h.get("errmsg")).and_then(Value::as_str).unwrap_or("")
                        ));
                    }
                    continue;
                }
                match frame.cmd.as_deref() {
                    Some(CMD_MSG_CALLBACK) => {
                        let req_id = frame.req_id();
                        if let Some(inbound) = parse_inbound(&frame.body, &req_id) {
                            port.anchors
                                .lock()
                                .await
                                .insert(inbound.user.clone(), req_id);
                            if events.send(inbound).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                    // 回执：errcode==0 才算这一帧真的到了。
                    _ => {
                        if frame.cmd.is_none() {
                            if frame.errcode == Some(0) {
                                missed_pong = 0;
                            } else if let Some(code) = frame.errcode {
                                tracing::warn!(errcode = code, "企业微信回执报错");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(msgtype: &str, extra: Value) -> Value {
        let mut v = json!({
            "msgid": "M1",
            "chatid": "C1",
            "chattype": "single",
            "msgtype": msgtype,
            "from": { "userid": "u-42" },
            "create_time": 1_700_000_000_000_i64,
        });
        if let (Some(dst), Value::Object(src)) = (v.as_object_mut(), extra) {
            for (k, val) in src {
                dst.insert(k, val);
            }
        }
        v
    }

    #[test]
    fn 解析单聊文本() {
        let body = single("text", json!({ "text": { "content": "  跑一下测试  " } }));
        let got = parse_inbound(&body, "req-1").expect("应可路由");
        assert_eq!(got.user, "u-42");
        assert_eq!(got.text, "跑一下测试");
        assert_eq!(got.dedup_key, "M1");
        assert_eq!(got.anchor, "req-1", "回复必须挂在入站 req_id 上");
    }

    #[test]
    fn 群消息被拒() {
        let mut body = single("text", json!({ "text": { "content": "hi" } }));
        body["chattype"] = json!("group");
        assert!(parse_inbound(&body, "r").is_none());
    }

    #[test]
    fn 无_userid_被拒() {
        let body = json!({ "msgid": "M", "chattype": "single", "msgtype": "text" });
        assert!(parse_inbound(&body, "r").is_none());
    }

    #[test]
    fn 语音转写作为正文() {
        let body = single("voice", json!({ "voice": { "content": "帮我重启" } }));
        assert_eq!(parse_inbound(&body, "r").unwrap().text, "帮我重启");
    }

    #[test]
    fn 图文混排只取文本段() {
        let body = single("mixed", json!({
            "mixed": { "msg_item": [
                { "msgtype": "text", "text": { "content": "看这张图" } },
                { "msgtype": "image" }
            ] }
        }));
        assert_eq!(parse_inbound(&body, "r").unwrap().text, "看这张图");
    }

    #[test]
    fn 引用回复带出原文() {
        let body = single("text", json!({
            "text": { "content": "这个怎么改" },
            "quote": { "text": { "content": "原始问题" } }
        }));
        let got = parse_inbound(&body, "r").unwrap();
        assert!(got.text.starts_with("> 原始问题"), "实际: {}", got.text);
        assert!(got.text.contains("这个怎么改"));
    }

    #[test]
    fn 无_msgid_时退化去重键() {
        let mut body = single("text", json!({ "text": { "content": "hi" } }));
        body["msgid"] = json!("");
        let got = parse_inbound(&body, "r").unwrap();
        assert!(got.dedup_key.contains("u-42"));
    }

    #[test]
    fn 按字节切分中文不溢出() {
        let text = "中".repeat(2000); // 6000 字节
        let parts = split_by_bytes(&text, TEXT_BYTE_LIMIT);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(utf8_len(p) <= TEXT_BYTE_LIMIT);
        }
        assert_eq!(parts.concat(), text, "切分必须无损");
    }

    #[test]
    fn 短文本不切() {
        assert_eq!(split_by_bytes("短", 100), vec!["短".to_string()]);
    }

    #[test]
    fn req_id_带前缀() {
        assert!(new_req_id("sub").starts_with("sub_"));
    }
}
