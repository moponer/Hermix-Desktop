//! QQ 官方 bot C2C 通道的协议层。
//!
//! 协议常量全部对着腾讯官方 SDK（`@tencent-connect/qqbot-nodejs`）的实现抄，
//! 不凭记忆写：网关是反向 WebSocket（`wss://api.sgroup.qq.com/...`），本机
//! 不需要公网地址，出站消息走 REST。
//!
//! 抄它、但不抄它的架构：官方 SDK 是 out-of-process adapter，要自己带一套
//! WS→本地服务端的桥接 + 令牌 + 会话映射文件。这里跑在 `hermix-server`
//! 进程内，所以整套桥接都不需要。

use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use super::port::{ImPort, ImStream, Inbound};

// ---------------------------------------------------------------------------
// 协议常量
// ---------------------------------------------------------------------------

const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const API_BASE: &str = "https://api.sgroup.qq.com";
const GATEWAY_PATH: &str = "/gateway";
const C2C_MESSAGE_PATH: &str = "/v2/users/{openid}/messages";
const C2C_STREAM_PATH: &str = "/v2/users/{openid}/stream_messages";

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;

/// `DIRECT_MESSAGE`（单聊）| `GROUP_AND_C2C`（群 + 单聊）。只处理 C2C，
/// 但意图位得一起开，否则网关不下发。
const INTENTS: u64 = (1 << 12) | (1 << 25);
const SHARD: [u8; 2] = [0, 1];

const EVENT_READY: &str = "READY";
const EVENT_C2C_MESSAGE_CREATE: &str = "C2C_MESSAGE_CREATE";

/// 重连退避。与官方 SDK 的 `RECONNECT_DELAYS` 一致。
const RECONNECT_DELAYS_MS: [u64; 6] = [1000, 2000, 5000, 10000, 30000, 60000];

/// 单条文本消息长度上限。超出按句号优先切，避免把一个词劈成两半。
const QQ_TEXT_LIMIT: usize = 1500;

/// 流式消息最小刷新间隔。QQ 侧有 300ms 下限，取 600ms 留一倍余量。
const STREAM_THROTTLE_MS: u64 = 600;

/// 令牌提前多久刷新。官方 SDK 用 `min(5min, 剩余/3)`，这里取固定值——
/// 少一个分支，而且 QQ 的令牌本来就是 2 小时量级。
const TOKEN_REFRESH_AHEAD: Duration = Duration::from_secs(300);
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(7200);

/// 走 `provider.rs:37` 那套超时口径：`Client::new()` 没有连接超时，
/// 半开 TCP 在 Windows 上会一直挂着。
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ---------------------------------------------------------------------------
// 入站消息
// ---------------------------------------------------------------------------

/// 从 `C2C_MESSAGE_CREATE` 的 `d` 字段抽出我们关心的部分。
///
/// 只处理 C2C：配对授权的是**一个人**，一条群消息等于把这份授权转发给
/// 群里所有其他人。
pub fn parse_inbound(d: &Value) -> Option<Inbound> {
    if d.get("author").and_then(|a| a.get("user_openid")).is_none() {
        return None;
    }
    let openid = d["author"]["user_openid"].as_str()?.trim();
    if openid.is_empty() {
        return None;
    }
    // 语音消息在 C2C 里以附件形式抵达，平台已经转成了文本。拼进正文，
    // 而不是当附件下载——Agent 读不了音频。
    let mut text = d
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    for att in d.get("attachments").and_then(Value::as_array).into_iter().flatten() {
        if let Some(asr) = att.get("asr_refer_text").and_then(Value::as_str) {
            let asr = asr.trim();
            if !asr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(asr);
            }
        }
    }
    let message_id = d.get("id").and_then(Value::as_str).unwrap_or("").trim();
    let timestamp = d
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let nickname = d
        .get("author")
        .and_then(|a| a.get("member_nick"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    // 没有平台 id 就退化成 身份+时间+开头 的组合，够去重了。
    let dedup_key = if message_id.is_empty() {
        format!(
            "{openid}:{timestamp}:{}",
            text.chars().take(16).collect::<String>()
        )
    } else {
        message_id.to_string()
    };

    Some(Inbound {
        user: openid.to_string(),
        display_name: if nickname.is_empty() {
            "QQ 用户".to_string()
        } else {
            nickname.to_string()
        },
        text,
        dedup_key,
        // QQ 的流式回复锚在入站 msg_id 上。
        anchor: message_id.to_string(),
    })
}

/// 一条网关下行帧。
#[derive(Debug, Deserialize)]
struct GatewayFrame {
    op: u8,
    /// 序列号。断线重连时带着它做 RESUME。
    #[serde(default)]
    s: Option<i64>,
    /// 事件名，只在 `op == DISPATCH` 时有意义。
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    d: Value,
}

/// 按长度上限切文本。优先在换行或句号处断，尽量不把词劈开。
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if cur.chars().count() >= limit {
            let cut = cur
                .rfind(['\n', '。', '！', '？', '.', '!', '?', ' '])
                .filter(|i| *i > cur.len() / 2)
                .unwrap_or(cur.len());
            let tail = cur.split_off(cut);
            out.push(cur);
            cur = tail;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn next_msg_seq() -> u32 {
    // 官方 SDK 的做法：时间戳低位异或一个随机数再取模。QQ 用它区分同一
    // 轮里发的多条流式帧，取模 65536 是协议规定的值域。
    let time_part = (chrono::Utc::now().timestamp_millis().unsigned_abs() % 100_000_000) as u32;
    let random = rand::random::<u32>() % 65_536;
    (time_part ^ random) % 65_536
}

// ---------------------------------------------------------------------------
// REST 客户端
// ---------------------------------------------------------------------------

struct CachedToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default, deserialize_with = "de_expires_in")]
    expires_in: Option<u64>,
}

/// QQ 的 `expires_in` 时而是数字、时而是字符串（实测返回 `"6580"`），
/// 写死 `Option<u64>` 会在字符串形态下整个 token 响应解析失败。
fn de_expires_in<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<Value> = Option::deserialize(d)?;
    Ok(match v {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// QQ 开放平台 REST 面（发消息、取网关地址、取令牌）。
pub struct QqApi {
    http: reqwest::Client,
    app_id: String,
    app_secret: String,
    token: tokio::sync::Mutex<Option<CachedToken>>,
}

impl QqApi {
    pub fn new(app_id: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            http: http_client(),
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            token: tokio::sync::Mutex::new(None),
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// 拿 access_token，带缓存和提前刷新。
    pub async fn access_token(&self) -> Result<String, String> {
        let mut guard = self.token.lock().await;
        if let Some(c) = guard.as_ref() {
            if Instant::now() < c.expires_at - TOKEN_REFRESH_AHEAD {
                return Ok(c.token.clone());
            }
        }
        let resp = self
            .http
            .post(TOKEN_URL)
            .json(&json!({ "appId": self.app_id, "clientSecret": self.app_secret }))
            .send()
            .await
            .map_err(|e| format!("取 access_token 网络错误: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("取 access_token 失败 HTTP {status}: {}", truncate(&body, 200)));
        }
        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("解析 access_token 响应失败: {e}"))?;
        let ttl = parsed
            .expires_in
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TOKEN_TTL);
        *guard = Some(CachedToken {
            token: parsed.access_token.clone(),
            expires_at: Instant::now() + ttl,
        });
        Ok(parsed.access_token)
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let token = self.access_token().await?;
        let url = format!("{API_BASE}{path}");
        let resp = self
            .http
            .get(&url)
            // QQ 开放平台的鉴权头是 `QQBot <token>`，不是标准 Bearer——
            // 用 bearer_auth 会 401「请求头Authorization参数格式错误」。
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await
            .map_err(|e| format!("GET {path} 网络错误: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("GET {path} HTTP {status}: {}", truncate(&body, 200)));
        }
        serde_json::from_str(&body).map_err(|e| format!("解析 {path} 响应失败: {e}"))
    }

    /// 网关地址。每次重连都重新取——它可能换。
    pub async fn gateway_url(&self) -> Result<String, String> {
        let v = self.get_json(GATEWAY_PATH).await?;
        v.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("网关地址响应缺少 url 字段: {v}"))
    }

    /// 发一条普通文本消息。`reply_to` 为空表示主动消息（不引用）。
    pub async fn send_text(
        &self,
        openid: &str,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<(), String> {
        let token = self.access_token().await?;
        let url = format!("{API_BASE}{}", C2C_MESSAGE_PATH.replace("{openid}", openid));
        for chunk in split_text(content, QQ_TEXT_LIMIT) {
            if chunk.trim().is_empty() {
                continue;
            }
            let mut body = json!({
                "content": chunk,
                "msg_type": 0,
                "msg_seq": next_msg_seq(),
            });
            if let Some(r) = reply_to {
                body["msg_id"] = json!(r);
            }
            self.post(&url, &token, &body).await?;
        }
        Ok(())
    }

    async fn post(&self, url: &str, token: &str, body: &Value) -> Result<Value, String> {
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("QQBot {token}"))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("POST {url} 网络错误: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("POST {url} HTTP {status}: {}", truncate(&text, 200)));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// 发一帧流式消息。`input_mode=replace` 语义：每帧是**全文替换**，
    /// 不是增量追加。
    async fn send_stream_frame(
        &self,
        openid: &str,
        event_id: &str,
        reply_to: &str,
        msg_seq: u32,
        index: u32,
        stream_msg_id: Option<&str>,
        content: &str,
        done: bool,
    ) -> Result<Option<String>, String> {
        let token = self.access_token().await?;
        let url = format!("{API_BASE}{}", C2C_STREAM_PATH.replace("{openid}", openid));
        let mut body = json!({
            "input_mode": "replace",
            // 1 = 生成中，10 = 定稿。
            "input_state": if done { 10 } else { 1 },
            "content_type": "markdown",
            "content_raw": content,
            "event_id": event_id,
            "msg_id": reply_to,
            "msg_seq": msg_seq,
            "index": index,
        });
        if let Some(id) = stream_msg_id {
            body["stream_msg_id"] = json!(id);
        }
        let resp = self.post(&url, &token, &body).await?;
        Ok(resp
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string))
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

// ---------------------------------------------------------------------------
// 流式回复
// ---------------------------------------------------------------------------

/// QQ 平台端口。持有 REST 客户端，网关和发送共用。
pub struct QqPort {
    api: std::sync::Arc<QqApi>,
}

impl QqPort {
    pub fn new(api: std::sync::Arc<QqApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl ImPort for QqPort {
    fn platform(&self) -> &'static str {
        "qq"
    }

    async fn send_text(&self, user: &str, content: &str, anchor: &str) -> Result<(), String> {
        let reply_to = if anchor.is_empty() {
            None
        } else {
            Some(anchor)
        };
        self.api.send_text(user, content, reply_to).await
    }

    fn start_stream(&self, user: &str, anchor: &str) -> Box<dyn ImStream> {
        Box::new(QqStream::new(self.api.clone(), user, anchor))
    }
}

/// 把一轮助手输出渲染成一条 QQ 流式消息（同一条消息随生成刷新）。
///
/// 流式接口只对 C2C 开放，而且锚在入站那条消息的 `msg_id` 上。拿不到锚点
/// （比如主动消息触发的轮次）时整轮退回普通分片消息——宁可丑，不可丢。
pub struct QqStream {
    api: std::sync::Arc<QqApi>,
    openid: String,
    reply_to: String,
    /// 进度行，显示在正文上方（工具调用轨迹）。空表示不显示。
    header: String,
    /// 本轮累计的全部文本。`input_mode=replace` 要求每帧发全文。
    accumulated: String,
    /// 已经显示出去的长度。`finish` 时只补这段尾巴，不重发整轮。
    shown: usize,
    msg_seq: u32,
    index: u32,
    stream_msg_id: Option<String>,
    last_flush: Option<Instant>,
    /// 一旦流式失败，本轮剩余部分改走普通消息。
    degraded: bool,
}

impl QqStream {
    pub fn new(api: std::sync::Arc<QqApi>, openid: &str, reply_to: &str) -> Self {
        Self {
            api,
            openid: openid.to_string(),
            reply_to: reply_to.to_string(),
            header: String::new(),
            accumulated: String::new(),
            shown: 0,
            msg_seq: next_msg_seq(),
            index: 0,
            stream_msg_id: None,
            last_flush: None,
            degraded: reply_to.is_empty(),
        }
    }

    /// 喂一段增量。内部按 `STREAM_THROTTLE_MS` 合并刷新。
    async fn append_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.accumulated.push_str(delta);
        if self.degraded {
            return;
        }
        let due = self
            .last_flush
            .map(|t| t.elapsed() >= Duration::from_millis(STREAM_THROTTLE_MS))
            .unwrap_or(true);
        if due {
            self.flush(false).await;
        }
    }
    async fn flush(&mut self, done: bool) {
        if self.degraded {
            return;
        }
        if !done && self.accumulated.chars().count() == self.shown {
            return;
        }
        let snapshot = self.rendered();
        let index = self.index;
        self.index += 1;
        match self
            .api
            .send_stream_frame(
                &self.openid,
                &self.reply_to,
                &self.reply_to,
                self.msg_seq,
                index,
                self.stream_msg_id.as_deref(),
                &snapshot,
                done,
            )
            .await
        {
            Ok(id) => {
                if self.stream_msg_id.is_none() {
                    self.stream_msg_id = id;
                }
                self.shown = snapshot.chars().count();
                self.last_flush = Some(Instant::now());
            }
            Err(e) => {
                // 断了就整轮降级：对一个已经坏掉的气泡继续重试，
                // 只会把本轮剩下的文字全丢掉。
                tracing::warn!(error = %e, "QQ 流式刷新失败，本轮剩余部分改用普通消息");
                self.degraded = true;
            }
        }
    }

    /// 收尾。流式正常就定稿；降级或失败就把气泡没显示过的那段补发。
    /// 进度行 + 正文。`input_mode=replace` 每帧发全文，所以这里拼一次就够。
    fn rendered(&self) -> String {
        if self.header.is_empty() {
            self.accumulated.clone()
        } else if self.accumulated.is_empty() {
            self.header.clone()
        } else {
            format!("{}

{}", self.header, self.accumulated)
        }
    }

    async fn complete(&mut self) {
        if !self.degraded {
            self.flush(true).await;
            if !self.degraded {
                return;
            }
        }
        let tail: String = self.rendered().chars().skip(self.shown).collect();
        let tail = tail.trim();
        if !tail.is_empty() {
            let _ = self
                .api
                .send_text(&self.openid, tail, Some(&self.reply_to))
                .await;
        }
    }
}

#[async_trait]
impl ImStream for QqStream {
    async fn append(&mut self, delta: &str) {
        self.append_delta(delta).await;
    }

    async fn set_header(&mut self, header: &str) {
        self.header = header.to_string();
    }

    async fn finish(&mut self) {
        self.complete().await;
    }
}

// ---------------------------------------------------------------------------
// 网关
// ---------------------------------------------------------------------------

/// 断线重连时保留的会话状态。跨进程重启不保留——那要落盘，
/// 换来的是"重启后几秒钟内的消息会丢"，不值得。
#[derive(Default)]
struct ResumeState {
    session_id: Option<String>,
    last_seq: Option<i64>,
}

/// 网关主循环。连不上就退避重连，收到事件就往 `events` 里送。
///
/// 整个循环由**一个** task 独占 socket。这里没有 JS 那套
/// "Mutex<WebSocketStream> + 跨 await 持锁"的问题，也不需要——
/// 心跳、读帧、停机三条线在一个 `select!` 里，谁也不阻塞谁。
pub async fn run_gateway(
    api: std::sync::Arc<QqApi>,
    events: mpsc::Sender<Inbound>,
    mut shutdown: watch::Receiver<bool>,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    let mut resume = ResumeState::default();
    let mut attempt = 0usize;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let result = connect_and_pump(&api, &events, &mut shutdown, &mut resume, connected.clone()).await;
        // 出 pumps 即断线（正常断开 / 报错 / 对端关闭都走这里），统一复位。
        // 状态接口用它回答"凭据在但网关没连上"这类不可见故障。
        connected.store(false, std::sync::atomic::Ordering::Relaxed);
        match result {
            Ok(()) => {
                // 正常断开：立刻重连。故障记录也一并清掉——上一条是旧的。
                if let Ok(mut g) = last_error.lock() {
                    *g = None;
                }
                attempt = 0;
            }
            Err(e) => {
                tracing::warn!(error = %e, "QQ 网关连接中断，准备重连");
                // 原文进状态接口，前端错误面板逐字给用户看。
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
    api: &std::sync::Arc<QqApi>,
    events: &mpsc::Sender<Inbound>,
    shutdown: &mut watch::Receiver<bool>,
    resume: &mut ResumeState,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    let url = api.gateway_url().await?;
    let access = api.access_token().await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| format!("连接网关 {url} 失败: {e}"))?;
    connected.store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(app_id = %api.app_id(), "QQ 网关已连接");

    // HELLO 一定是对端的第一帧，里面带心跳周期。读不到就当握手失败。
    let first = tokio::time::timeout(Duration::from_secs(15), ws.next())
        .await
        .map_err(|_| "等待 HELLO 超时".to_string())?
        .ok_or_else(|| "网关在建连后立刻关闭".to_string())?;
    let first = first.map_err(|e| format!("读取 HELLO 失败: {e}"))?;
    let hello: GatewayFrame =
        serde_json::from_str(&frame_text(&first)?).map_err(|e| format!("解析 HELLO 失败: {e}"))?;
    if hello.op != OP_HELLO {
        return Err(format!("网关首帧不是 HELLO，而是 op={}", hello.op));
    }
    let hb_ms = hello
        .d
        .get("heartbeat_interval")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .max(5_000);

    // 有会话就 RESUME，断线期间的消息能补回来；没有就 IDENTIFY。
    let identify = match (&resume.session_id, resume.last_seq) {
        (Some(sid), Some(seq)) => json!({
            "op": OP_RESUME,
            "d": { "token": format!("QQBot {access}"), "session_id": sid, "seq": seq }
        }),
        _ => json!({
            "op": OP_IDENTIFY,
            "d": { "token": format!("QQBot {access}"), "intents": INTENTS, "shard": SHARD }
        }),
    };
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .map_err(|e| format!("发送 IDENTIFY 失败: {e}"))?;

    let mut heartbeat = tokio::time::interval(Duration::from_millis(hb_ms));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                let _ = ws.close(None).await;
                return Ok(());
            }
            _ = heartbeat.tick() => {
                let hb = json!({ "op": OP_HEARTBEAT, "d": resume.last_seq });
                if ws.send(Message::Text(hb.to_string().into())).await.is_err() {
                    return Err("发送心跳失败".into());
                }
            }
            next = ws.next() => {
                let Some(next) = next else { return Ok(()) };
                let text = match next {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Ping(p)) => { let _ = ws.send(Message::Pong(p)).await; continue; }
                    Ok(Message::Close(_)) => return Ok(()),
                    Ok(_) => continue,
                    Err(e) => return Err(format!("网关读帧失败: {e}")),
                };
                let frame: GatewayFrame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => { tracing::warn!(error = %e, "网关帧解析失败，忽略"); continue; }
                };
                if let Some(s) = frame.s { resume.last_seq = Some(s); }
                match frame.op {
                    OP_DISPATCH => {
                        let t = frame.t.as_deref().unwrap_or("");
                        if t == EVENT_READY {
                            resume.session_id = frame
                                .d
                                .get("session_id")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            tracing::info!(session_id = ?resume.session_id, "QQ 网关就绪");
                        } else if t == EVENT_C2C_MESSAGE_CREATE {
                            if let Some(inbound) = parse_inbound(&frame.d) {
                                // 接收端堵了就丢：一条私聊消息不值得把整个
                                // 网关循环卡住，下一条会补上。
                                if events.send(inbound).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    // 这三个都要重连。重连时如果 session_id 还在，走 RESUME。
                    OP_RECONNECT => return Ok(()),
                    OP_INVALID_SESSION => {
                        if !frame.d.as_bool().unwrap_or(false) {
                            resume.session_id = None;
                            resume.last_seq = None;
                        }
                        // 走 Err 而不是 Ok：让 run_gateway 把原因记进
                        // last_error，前端错误面板才能显示真实原因，
                        // 否则「网关连接失败」没有任何 detail，误导排查方向。
                        return Err(
                            "网关拒收会话（INVALID_SESSION）：token 无效或 intents 未在开放平台勾选"
                                .into(),
                        );
                    }
                    OP_HEARTBEAT => {
                        let hb = json!({ "op": OP_HEARTBEAT, "d": resume.last_seq });
                        let _ = ws.send(Message::Text(hb.to_string().into())).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn frame_text(msg: &Message) -> Result<String, String> {
    match msg {
        Message::Text(t) => Ok(t.to_string()),
        Message::Binary(b) => String::from_utf8(b.to_vec())
            .map_err(|e| format!("网关二进制帧不是 UTF-8: {e}")),
        other => Err(format!("意外的帧类型: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_c2c() -> Value {
        json!({
            "id": "MSG001",
            "timestamp": "2026-09-29T10:00:00+08:00",
            "content": "看看 main.rs",
            "author": { "user_openid": "openid-abc", "member_nick": "阿伟" },
            "attachments": []
        })
    }

    #[test]
    fn 解析正常单聊消息() {
        let got = parse_inbound(&sample_c2c()).expect("应解析出消息");
        assert_eq!(got.user, "openid-abc");
        assert_eq!(got.text, "看看 main.rs");
        assert_eq!(got.dedup_key, "MSG001");
        assert_eq!(got.anchor, "MSG001", "流式回复锚在入站 msg_id 上");
        assert_eq!(got.display_name, "阿伟");
    }

    #[test]
    fn 语音转写并入正文() {
        let mut v = sample_c2c();
        v["content"] = json!("");
        v["attachments"] = json!([{ "asr_refer_text": "帮我重启服务" }]);
        let got = parse_inbound(&v).expect("语音消息也走文本路径");
        assert_eq!(got.text, "帮我重启服务");
    }

    #[test]
    fn 缺_openid_的事件被拒() {
        let v = json!({ "id": "x", "content": "hi" });
        assert!(parse_inbound(&v).is_none(), "没有 author 就不是可路由的私聊");
    }

    #[test]
    fn 空白_openid_被拒() {
        let v = json!({ "author": { "user_openid": "   " }, "id": "x" });
        assert!(parse_inbound(&v).is_none());
    }

    #[test]
    fn 无平台_id_时退化成去重键() {
        let mut v = sample_c2c();
        v["id"] = json!("");
        let got = parse_inbound(&v).expect("仍应可路由");
        assert!(got.dedup_key.contains("openid-abc"));
        assert!(!got.dedup_key.is_empty());
    }

    #[test]
    fn 昵称缺失时给占位() {
        let mut v = sample_c2c();
        v["author"] = json!({ "user_openid": "openid-abc" });
        let got = parse_inbound(&v).unwrap();
        assert_eq!(got.display_name, "QQ 用户");
    }

    #[test]
    fn 短文本不切() {
        assert_eq!(split_text("短", 100), vec!["短".to_string()]);
    }

    #[test]
    fn 长文本切分后不丢字() {
        let long = "句子。".repeat(1000);
        let parts = split_text(&long, QQ_TEXT_LIMIT);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(p.chars().count() <= QQ_TEXT_LIMIT, "每段都不该超限");
        }
        assert_eq!(parts.concat(), long, "切分必须无损");
    }

    #[test]
    fn 网关帧解析() {
        let f: GatewayFrame =
            serde_json::from_str(r#"{"op":0,"s":42,"t":"C2C_MESSAGE_CREATE","d":{}}"#).unwrap();
        assert_eq!(f.op, OP_DISPATCH);
        assert_eq!(f.s, Some(42));
        assert_eq!(f.t.as_deref(), Some(EVENT_C2C_MESSAGE_CREATE));
    }

    #[test]
    fn 消息序号落在协议值域内() {
        for _ in 0..100 {
            assert!(next_msg_seq() < 65_536);
        }
    }
}
