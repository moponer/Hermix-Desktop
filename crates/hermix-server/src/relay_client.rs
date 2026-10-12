//! 出站 relay 连接器：PC 主动连自建 relay，等手机的帧。
//!
//! **为什么是出站**：手机在 NAT 后面，PC 可能在 NAT 后面，也可能没有公网入口。
//! 两边都主动连中间人，是唯一「任何网络都通」且不需要打洞的形态（与 IM 通道
//! 「双方主动连中间人」同一模型）。
//!
//! **relay 不是可选项**（2026-10-04 拍板）：手机端唯一通道，没有直连降级。
//! 代价是 relay 成了单点——但**协议层保持 base-URL 可替换**：本模块只认
//! 配置里的 URL，不把 relay 的路由假设写进业务代码，将来若要改，改配置即可。
//!
//! **不配置 relay 时整个模块不启动**，桌面端行为零变化——便携零安装产品的
//! 默认形态是「没开远程」，不是「开了但连不上」。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::relay_proto::{Envelope, FrameTag, PROTO_VERSION};

/// relay 连接配置。留空 = 不启用远程入口。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelayConfig {
    /// relay 根地址，如 `wss://relay.example.com`。空 = 不连。
    #[serde(default)]
    pub url: String,
    /// 本机在 relay 侧的稳定标识（一般是设备 id）。
    #[serde(default)]
    pub node_id: String,
    /// 首连前的等待。默认 2s。
    #[serde(default = "default_initial_backoff")]
    pub initial_backoff_secs: u64,
    /// 退避上限。默认 60s。
    #[serde(default = "default_max_backoff")]
    pub max_backoff_secs: u64,
}

fn default_initial_backoff() -> u64 {
    2
}
fn default_max_backoff() -> u64 {
    60
}

impl RelayConfig {
    /// 没配地址就等于「不启用」，不是「配错了」。
    pub fn enabled(&self) -> bool {
        !self.url.trim().is_empty()
    }

    /// 指数退避的第 n 次等待，上限 `max_backoff_secs`。
    ///
    /// 上限是**必需的**：relay 长时间不可用时，无上限的退避会退到几十分钟，
    /// 于是「relay 恢复后自动接上」这个最朴素的用户预期就不成立了。
    pub fn backoff(&self, attempt: u32) -> Duration {
        let base = self.initial_backoff_secs.max(1);
        let secs = base.saturating_mul(1u64 << attempt.min(16));
        Duration::from_secs(secs.min(self.max_backoff_secs.max(base)))
    }
}

/// 一条从手机侧来的待处理帧。
///
/// 刻意只带**已解析的信封**：内层 payload 不在这里解码——解码归
/// `mobile_protocol`，避免同一个 envelope 有两处解析。
#[derive(Debug, Clone, PartialEq)]
pub struct InboundFrame {
    pub envelope: Envelope,
}

/// 连接生命周期状态。给 `/api/channels/status` 那类诊断面用，也是日志的锚。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayState {
    /// 未配置 relay —— 默认形态。
    Disabled,
    /// 正在首连 / 重连。
    Connecting,
    /// 已登记，等帧。
    Connected,
}

/// 共享的连接状态，供诊断与 UI 读取。
#[derive(Debug, Clone)]
pub struct RelayStatus {
    inner: Arc<std::sync::Mutex<RelayStatusInner>>,
}

#[derive(Debug, Default, Clone)]
struct RelayStatusInner {
    state: Option<RelayState>,
    attempts: u32,
}

impl Default for RelayStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayStatus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(RelayStatusInner {
                state: Some(RelayState::Disabled),
                attempts: 0,
            })),
        }
    }

    pub fn state(&self) -> RelayState {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .unwrap_or(RelayState::Disabled)
    }

    pub fn attempts(&self) -> u32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).attempts
    }

    fn set(&self, s: RelayState) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.state = Some(s);
        if s == RelayState::Connected {
            g.attempts = 0;
        }
    }

    fn bump_attempt(&self) -> u32 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.state = Some(RelayState::Connecting);
        g.attempts = g.attempts.saturating_add(1);
        g.attempts
    }
}

/// 出站连接循环的骨架：重连 + 退避。
///
/// 真实连接由 [`connect_and_serve`] 提供，单独抽出来是为了**退避逻辑可测**——
/// 退避错（无上限 / 不重置）会让 relay 恢复后接不上，而那种故障在现场极难复现。
pub async fn connect_and_serve<F, Fut>(cfg: RelayConfig, status: RelayStatus, mut one_shot: F)
where
    F: FnMut(RelayConfig) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if !cfg.enabled() {
        status.set(RelayState::Disabled);
        return;
    }
    loop {
        let attempt = status.bump_attempt();
        one_shot(cfg.clone()).await;
        if !cfg.enabled() {
            return;
        }
        let wait = cfg.backoff(attempt);
        tracing::info!(
            attempt,
            wait_secs = wait.as_secs(),
            "relay 连接断开，退避后重连"
        );
        tokio::time::sleep(wait).await;
    }
}

/// 登记帧：PC 出站连上后告诉 relay「我是谁、我支持什么版本」。
pub fn hello_frame(cfg: &RelayConfig) -> Envelope {
    Envelope {
        token_hash: None,
        node_id: Some(cfg.node_id.clone()),
        ..Envelope::new(FrameTag::Hello)
    }
}

/// 解析一帧，判断它是不是**调用方需要处理**的帧。
///
/// 版本不符一律拒绝，**不降级**：错位的帧会一路透传到 agent，表现为「能连上
/// 但消息莫名消失」，比直接失败难查一个量级。
///
/// `Close` **要放行**：那是手机挂断某条会话的信号，被当成"不是业务帧"丢掉，
/// 挂断就永远不会到达 PC，会话会一直挂着。
pub fn accept_inbound(raw: &str) -> Option<InboundFrame> {
    let envelope: Envelope = serde_json::from_str(raw).ok()?;
    if !envelope.version_ok() {
        tracing::warn!(
            got = envelope.proto_version,
            want = PROTO_VERSION,
            "relay 协议版本不符，拒绝"
        );
        return None;
    }
    match envelope.tag {
        // 业务帧与挂断帧都要交给调用方
        FrameTag::Route | FrameTag::Close => Some(InboundFrame { envelope }),
        // Hello / Ack 是链路自身的握手帧，不是业务数据。
        FrameTag::Hello | FrameTag::Ack => None,
    }
}

/// 建立到 relay 的出站 WS，并跑起收/发两个方向。
///
/// 一条连接 = 一台电脑。手机上可能同时开着几条会话，进来��帧按
/// `session_id` 拆给各自的 `handle_ws_stream`（见 `relay_transport`）。
///
/// **鉴权在拆包之前**（`verify_frame_auth`），顺序反了就等于允许改
/// `session_id` 冒用别人的会话。
///
/// `token` 是 **relay 凭据**（`hrel_*`），不是 hermix 设备 token——这一跳只
/// 过了 relay，hermix 侧的凭据在帧里（`FrameAuth`）。
pub async fn serve_relay_socket(
    cfg: RelayConfig,
    token: String,
    state: Arc<crate::state::HttpServerState>,
) -> std::io::Result<()> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    let url = relay_ws_url(&cfg.url);
    // relay 凭据必须随**握手请求**发出去（`Authorization: Bearer hrel_*`），
    // 否则 relay 直接 401 这条连接。只在帧里带是不够的——relay 认的是
    // 连接，不是帧。
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(std::io::Error::other)?;
    let auth = format!("Bearer {token}");
    req.headers_mut().insert(
        "authorization",
        auth.parse().map_err(std::io::Error::other)?,
    );
    // connect_async 返回 (WebSocketStream, http::Response) —— 顺序别搞反，
    // 搞反了会把 Response 当成流去 .next()。
    let (ws, resp) = tokio_tungstenite::connect_async(req).await.map_err(|e| {
        // 401 单独点名：调用方靠这个字符串识别「凭据被 relay 拒绝」并触发重签，
        // 与网络故障（退避重连即可）区分开。
        if let tokio_tungstenite::tungstenite::Error::Http(r) = &e {
            if r.status().as_u16() == 401 {
                return std::io::Error::other("relay 握手被拒（HTTP 401）：凭据无效或已撤销");
            }
        }
        std::io::Error::other(e)
    })?;
    tracing::info!(
        relay = %cfg.url,
        node = %cfg.node_id,
        status = %resp.status(),
        "已连上 relay"
    );

    // 一条 socket 要同时推流与收流（handler 在推的同时必须能读到手机的
    // out-of-band 消息），所以拆两半而不是一把锁串行。
    let (mut out_sink, mut in_stream) = ws.split();

    // 登记帧：告诉 relay 自己是哪台电脑 + 线路版本
    if let Ok(text) = serde_json::to_string(&hello_frame(&cfg)) {
        out_sink.send(Message::Text(text.into())).await.ok();
    }

    // 出站方向：各会话的 Transport 把帧塞进来，统一发到 relay
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Envelope>(256);
    tokio::spawn(async move {
        while let Some(env) = out_rx.recv().await {
            match serde_json::to_string(&env) {
                Ok(text) => {
                    if out_sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "relay 出站帧序列化失败"),
            }
        }
    });

    let mut mux = crate::relay_transport::SessionMux::new();
    let mut sessions: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    while let Some(msg) = in_stream.next().await {
        let Ok(msg) = msg else { break };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Close(_) => break,
            // Ping/Pong 由 tungstenite 自动应答，不进业务流
            _ => continue,
        };
        let Some(frame) = accept_inbound(&text) else {
            continue;
        };
        let env = frame.envelope;
        let Some(sid) = env.session_id.clone() else {
            tracing::warn!("relay 帧缺 session_id，已丢弃");
            continue;
        };

        // 配对会话必须在常规帧鉴权**之前**分派：配对时手机还没有设备凭据，
        // verify_frame_auth 必然拒绝。信任锚见 pairing::PAIRING_SESSION_ID。
        if env.tag == FrameTag::Route && sid == crate::pairing::PAIRING_SESSION_ID {
            let st = state.clone();
            tokio::spawn(crate::pairing::handle_pairing_frame(st, env, out_tx.clone()));
            continue;
        }

        // 鉴权先于拆包 —— 顺序是安全属性，不是风格。
        if let Err(why) = crate::mobile_ws::verify_frame_auth(&state, &env) {
            tracing::warn!(session = %sid, reason = %why, "relay 帧鉴权失败");
            continue;
        }

        match env.tag {
            FrameTag::Close => {
                tracing::info!(session = %sid, "对端挂断该会话");
                mux.remove(&sid);
                if let Some(h) = sessions.remove(&sid) {
                    h.abort();
                }
            }
            FrameTag::Route => {
                if let Some(payload) = env.payload.clone() {
                    let sink = mux.sink_for(&sid);
                    // 队列满：报错而不是静默丢（静默丢会让两边都以为发出去了）
                    if let Err(e) = sink.try_send(payload) {
                        tracing::warn!(session = %sid, error = %e, "会话入队失败");
                    }
                }
                // 首帧时起这个会话的 handle_ws_stream。transport 与上面的
                // sink 共用 mux 里那条通道——各自建一条的话消息永远到不了。
                if !sessions.contains_key(&sid) {
                    let transport = mux.transport_for(&sid, out_tx.clone());
                    let st = state.clone();
                    let sid_for_task = sid.clone();
                    sessions.insert(
                        sid.clone(),
                        tokio::spawn(async move {
                            crate::ws::handle_ws_stream(
                                transport,
                                (*st).clone(),
                                sid_for_task,
                            )
                            .await;
                        }),
                    );
                }
            }
            FrameTag::Hello | FrameTag::Ack => {}
        }
    }

    // 连接断了：所有会话的 task 一并收掉，否则它们会永远等一个没了的 socket。
    for (_, h) in sessions.drain() {
        h.abort();
    }
    tracing::info!("relay 连接结束");
    Ok(())
}

/// 拼出 relay 的 WS 地址。只认 `ws` / `wss`；`https://` 自动换成 `wss://`，
/// 否则用户按习惯填 https 会直接握手失败，而现象是"填了地址连不上"。
fn relay_ws_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.starts_with("wss://") {
        return format!("{b}/relay/node");
    }
    if b.starts_with("ws://") {
        return format!("{b}/relay/node");
    }
    if b.starts_with("https://") {
        return format!("wss://{}/relay/node", &b["https://".len()..]);
    }
    if b.starts_with("http://") {
        return format!("ws://{}/relay/node", &b["http://".len()..]);
    }
    format!("wss://{b}/relay/node")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RelayConfig {
        RelayConfig {
            url: "wss://relay.example.com".into(),
            node_id: "node-1".into(),
            initial_backoff_secs: 2,
            max_backoff_secs: 60,
        }
    }

    #[test]
    fn 空地址等于不启用() {
        let c = RelayConfig {
            url: "  ".into(),
            ..cfg()
        };
        assert!(!c.enabled(), "空白地址不是「配错」，是「没开」");
        assert!(cfg().enabled());
    }

    #[test]
    fn 退避指数增长且有上限() {
        let c = cfg();
        assert_eq!(c.backoff(0).as_secs(), 2);
        assert_eq!(c.backoff(1).as_secs(), 4);
        assert_eq!(c.backoff(2).as_secs(), 8);
        // 上限必须真的兜住：无上限的退避会让 relay 恢复后接不上
        assert_eq!(c.backoff(20).as_secs(), 60);
        // shift 溢出也不能变负或 panic
        assert!(c.backoff(u32::MAX).as_secs() <= 60);
    }

    #[tokio::test]
    async fn 未配置时一次都不连() {
        let status = RelayStatus::new();
        // 闭包是 FnMut，async 块里捕获的局部变量不能逃逸出去——用原子计数。
        let called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = called.clone();
        connect_and_serve(
            RelayConfig::default(),
            status.clone(),
            move |_| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            },
        )
        .await;
        assert_eq!(
            called.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "没配 relay 就不该有任何连接尝试"
        );
        assert_eq!(status.state(), RelayState::Disabled);
    }

    #[tokio::test]
    async fn 连上后重置重试计数() {
        let s = RelayStatus::new();
        s.bump_attempt();
        s.bump_attempt();
        assert_eq!(s.attempts(), 2);
        s.set(RelayState::Connected);
        assert_eq!(s.attempts(), 0, "连上就得清零，否则下次断线从高位退避起");
        assert_eq!(s.state(), RelayState::Connected);
    }

    #[test]
    fn 只收业务帧与挂断帧() {
        let ok = serde_json::to_string(&Envelope::route("{}".into())).unwrap();
        let got = accept_inbound(&ok).expect("route 帧应被接收");
        assert_eq!(got.envelope.tag, FrameTag::Route);

        let hello = serde_json::to_string(&hello_frame(&cfg())).unwrap();
        assert!(
            accept_inbound(&hello).is_none(),
            "握手帧不是业务数据，不该进处理路径"
        );
        let ack = serde_json::to_string(&Envelope::new(FrameTag::Ack)).unwrap();
        assert!(accept_inbound(&ack).is_none());

        // 挂断必须放行：被当成"非业务帧"丢掉的话，手机挂断永远到不了 PC
        let close = serde_json::to_string(&Envelope {
            session_id: Some("s1".into()),
            ..Envelope::new(FrameTag::Close)
        })
        .unwrap();
        let got = accept_inbound(&close).expect("挂断帧应被接收");
        assert_eq!(got.envelope.tag, FrameTag::Close);
        assert_eq!(got.envelope.session_id.as_deref(), Some("s1"));

        let bad = r#"{"proto_version":999,"tag":"route"}"#;
        assert!(accept_inbound(bad).is_none(), "版本不符必须拒绝");
        assert!(accept_inbound("not json").is_none());
    }

    #[test]
    fn relay地址拼接受输入写法影响() {
        // 用户按习惯填 https，直接当 ws 用会握手失败，而现象是"填了连不上"
        assert_eq!(
            relay_ws_url("https://relay.example.com"),
            "wss://relay.example.com/relay/node"
        );
        assert_eq!(
            relay_ws_url("https://relay.example.com/"),
            "wss://relay.example.com/relay/node"
        );
        assert_eq!(
            relay_ws_url("wss://relay.example.com"),
            "wss://relay.example.com/relay/node"
        );
        assert_eq!(
            relay_ws_url("http://127.0.0.1:8787"),
            "ws://127.0.0.1:8787/relay/node"
        );
        // 裸 host 补 wss（外网默认走 TLS，不给"明文上公网"留口子）
        assert_eq!(relay_ws_url("relay.example.com"), "wss://relay.example.com/relay/node");
    }

    #[test]
    fn 登记帧带本机标识() {
        let h = hello_frame(&cfg());
        assert_eq!(h.tag, FrameTag::Hello);
        assert_eq!(h.node_id.as_deref(), Some("node-1"));
        assert!(h.version_ok());
        // 登记帧不带 token —— 手机端的 token 不该出现在 PC 出站这一侧
        assert!(h.token_hash.is_none());
    }
}
