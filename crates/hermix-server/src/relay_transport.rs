//! PC 侧的 relay 连接：把一条出站 WS 上跑的多条会话，接回 `handle_ws_stream`。
//!
//! ## 为什么需要一个 demux
//!
//! 一条 relay 连接是**一台电脑**（按 `node_id` 选路），而电脑上可能同时有
//! 几条会话（手机开两个对话）。relay 只按 token 哈希选到这台电脑，之后的帧
//! **原封不动**转过来——它不拆包，也不管里面有谁。所以 PC 侧必须按信封上的
//! `session_id` 把它拆开，各喂各的 `handle_ws_stream`。
//!
//! ## 为什么鉴权在帧里
//!
//! App 不直接向 hermix 发 HTTP，它发 WS 帧。`Authorization` 与
//! `X-Hermix-Signature` 两个请求头**没有落脚的地方**，所以由帧的 `auth`
//! 字段携带，在这里、也就是 PC 侧，用与直连 `/v1/ws-stream` **同一份**
//! `authenticate_device_request` 判定（`mobile_ws::verify_frame_auth`）。
//! relay 不参与：它只透传，也不校验 token 真伪。
//!
//! ## 鉴权先于 demux
//!
//! 顺序不能反：**先验签、再分路**。反过来的话，攻击者只要改 `session_id`
//! 就能把帧送进别人的会话——而 `session_id` 恰好是重建签名路径要用的那个字段。
//! 先验签正是为了让签名与 `session_id` 绑定，从而堵住这条路。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::relay_proto::{Envelope, FrameTag};
use crate::transport::{Transport, TransportResult, TransportSink, TransportSource};

/// 一条会话的入站队列。上限是有意的：手机断线时不至于把 PC 的内存吃光。
const INBOUND_CAP: usize = 256;

/// 一条会话在 PC 侧的 `Transport`。
///
/// 它把 `handle_ws_stream` 的双向读写翻译成 relay 帧：
/// - `recv`：从入站队列取一帧内层文本；
/// - `send`：包成 `Envelope::route_for(sid, text)` 发回 relay。
pub struct RelayTransport {
    session_id: String,
    inbound_tx: mpsc::Sender<String>,
    inbound_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
    outbound_tx: mpsc::Sender<Envelope>,
}

impl RelayTransport {
    /// 用**已有**的入站通道构造一条 Transport。
    ///
    /// 必须与 `SessionMux` 持有的是同一条通道——各自建一条的话，投进 mux
    /// 的帧永远到不了 transport，而这种错误不报任何错（编译过、测试也过），
    /// 症状是「手机发的消息 agent 收不到」。
    pub fn over_channel(
        session_id: String,
        outbound_tx: mpsc::Sender<Envelope>,
        inbound: Inbound,
    ) -> Self {
        let (inbound_tx, inbound_rx) = inbound;
        Self {
            session_id,
            inbound_tx,
            inbound_rx,
            outbound_tx,
        }
    }

    fn new(session_id: String, outbound_tx: mpsc::Sender<Envelope>) -> (Self, mpsc::Sender<String>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAP);
        let rx = Arc::new(tokio::sync::Mutex::new(inbound_rx));
        (
            Self {
                session_id,
                inbound_tx: inbound_tx.clone(),
                inbound_rx: rx,
                outbound_tx,
            },
            inbound_tx,
        )
    }
}

impl Transport for RelayTransport {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>) {
        let session_id = self.session_id.clone();
        let outbound_tx = self.outbound_tx.clone();
        let inbound_tx = self.inbound_tx.clone();
        (
            Box::new(RelaySink {
                session_id,
                outbound_tx,
            }),
            Box::new(RelaySource {
                inbound_rx: self.inbound_rx,
                inbound_tx,
            }),
        )
    }
}

struct RelaySink {
    session_id: String,
    outbound_tx: mpsc::Sender<Envelope>,
}

#[async_trait::async_trait]
impl TransportSink for RelaySink {
    async fn send(&mut self, text: String) -> TransportResult<()> {
        // **relay 边界上做事件名映射**：handler 说的是 hermix 的
        // `ProtocolMessage`（stdio 那头的老协议），手机说的是 App 侧事件名。
        // 转换只在这一处发生，两种协议各说各的，改一边不影响另一边。
        //
        // 映射不出来的帧（例如桌面端专属的内部事件）直接**不转发**——给 App
        // 一个它不认识的 type，UI 会静默忽略，现场表现是「消息莫名消失」。
        let Some(mut mapped) = crate::mobile_protocol::to_mobile_frame(&parse_json(&text)?)
        else {
            tracing::debug!("该帧不面向 App，未转发");
            return Ok(());
        };
        // 审批请求要显式带上"永久放行不可用"，否则 App 只能静默少一个按钮
        if mapped.get("type").and_then(|t| t.as_str()) == Some("approval.request") {
            crate::mobile_protocol::set_can_persist_false(&mut mapped);
        }
        let payload = serde_json::to_string(&mapped).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        self.outbound_tx
            .send(Envelope::route_for(&self.session_id, payload))
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        // 关的是**这一条会话**在 relay 上的视图；出站方向结束用 close 帧，
        // 不拆整机连接（其它会话还在跑）。
        let _ = self
            .outbound_tx
            .send(Envelope {
                session_id: Some(self.session_id.clone()),
                ..Envelope::new(FrameTag::Close)
            })
            .await;
        Ok(())
    }
}

struct RelaySource {
    inbound_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
    /// 保留发送端：否则 `handle_ws_stream` 一放开 Recv 就会看到通道关闭，
    /// 把"暂时没有新帧"误判成"对端已关闭"。
    inbound_tx: mpsc::Sender<String>,
}

#[async_trait::async_trait]
impl TransportSource for RelaySource {
    async fn recv(&mut self) -> Option<String> {
        // 发送端由本结构体持有（见字段注释），所以它会一直 await 到真有帧，
        // 而不是把"暂时没消息"当成"对端已关闭"。
        self.inbound_rx.lock().await.recv().await
    }
}

/// 一条 relay 连接上的会话表。
///
/// 分路逻辑单独抽出来是为了能单测：多会话不串、未知 sid 不误投、
/// 已结束的会话不再重建。
#[derive(Default)]
pub struct SessionMux {
    entries: HashMap<String, Inbound>,
}

/// 解析 handler 发来的 JSON 文本帧。
fn parse_json(text: &str) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::from_str(text).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
}

/// 会话表里的一路：发送端给 `dispatch` 用，接收端包 `Arc` 以便克隆。
///
/// （`RelayTransport::new` 用的是裸通道——它同时独占两端，不需要共享。）
type Inbound = (mpsc::Sender<String>, Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>);

fn new_inbound() -> Inbound {
    let (tx, rx) = mpsc::channel(INBOUND_CAP);
    (tx, Arc::new(tokio::sync::Mutex::new(rx)))
}

impl SessionMux {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取一条会话的 `Transport`，**与 `dispatch` 用同一条通道**。
    ///
    /// 这是读帧循环与 handler 之间的唯一接缝：读循环调它拿到 transport 起
    /// `handle_ws_stream`，同一个 sid 的后续帧经 `dispatch` 投进去。
    /// 重复调用返回等价的 transport（同一通道），不会另建一条。
    pub fn transport_for(
        &mut self,
        sid: &str,
        outbound_tx: mpsc::Sender<Envelope>,
    ) -> RelayTransport {
        let inbound = self
            .entries
            .entry(sid.to_string())
            .or_insert_with(new_inbound)
            .clone();
        RelayTransport::over_channel(sid.to_string(), outbound_tx, inbound)
    }

    /// 取出（或首次为）某条会话的入队句柄。
    pub fn sink_for(&mut self, sid: &str) -> mpsc::Sender<String> {
        self.entry(sid)
    }

    /// 与 `sink_for` 相同，但**同时把接收端交给调用方**。
    ///
    /// 单测需要读到投进来的内容。接收端存在 `Arc<Mutex<..>>` 里就是为了
    /// 能被克隆——`mpsc::Receiver` 本身不可克隆，存不进可重复取的表。
    /// 生产路径用 `sink_for` 就够。
    pub fn pair_for(
        &mut self,
        sid: &str,
    ) -> (
        mpsc::Sender<String>,
        Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
    ) {
        self.entries
            .entry(sid.to_string())
            .or_insert_with(new_inbound)
            .clone()
    }

    fn entry(&mut self, sid: &str) -> mpsc::Sender<String> {
        self.entries
            .entry(sid.to_string())
            .or_insert_with(new_inbound)
            .0
            .clone()
    }

    /// 会话是否已被登记。
    pub fn contains(&self, sid: &str) -> bool {
        self.entries.contains_key(sid)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 一条会话结束（`handle_ws_stream` 返回）时摘掉它——否则 relay 一次
    /// 断线重连就在表里堆一份永不消费的死队列。
    pub fn remove(&mut self, sid: &str) {
        self.entries.remove(sid);
    }

    /// 把一帧**已验签**的入站帧投给对应会话。
    ///
    /// 鉴权**不**在这里做（`mobile_ws::verify_frame_auth` 已在投之前验过）。
    /// 缺 `session_id` 的帧直接丢：投给"随便某条会话"比丢掉危险。
    pub fn dispatch(&mut self, env: &Envelope) -> bool {
        if env.tag != FrameTag::Route {
            return false;
        }
        let (Some(sid), Some(payload)) = (env.session_id.as_deref(), env.payload.as_deref()) else {
            tracing::warn!("relay 帧缺 session_id 或 payload，已丢弃");
            return false;
        };
        // 会话不在表里 = 还没起（首帧竞态）→ 建一个。已结束的那次不会再有帧。
        let sink = self.sink_for(sid);
        // spawn 的 `async move` 会把捕获物带出本方法，而 `sid` / `payload`
        // 都是从 `env` 借来的 `&str`——跨 await 的 future 不能带借用
        // （E0521）。所以先把两者转成自有字符串。
        let sid_owned = sid.to_string();
        let payload_owned = payload.to_string();
        // 另给同步分支留一份 sink（spawn 那支把它移走了）。
        let sync_sink = sink.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    // 队列满 = 手机灌得太快或 PC 卡住。**报错而不是悄悄丢**：
                    // 静默丢会让两边都以为发出去了。
                    if let Err(e) = sink.send(payload_owned).await {
                        tracing::warn!(session = %sid_owned, error = %e, "会话入队失败（已满或已结束）");
                    }
                });
            }
            Err(_) => {
                // 不在 tokio 运行时里：同步尝试入队。
                let _ = sync_sink.try_send(payload_owned);
            }
        }
        true
    }
}

/// 组装一条会话的 `Transport` 与它的入队句柄（`SessionMux` 之外的便捷入口）。
pub fn new_relay_transport(
    session_id: &str,
    outbound_tx: mpsc::Sender<Envelope>,
) -> (RelayTransport, mpsc::Sender<String>) {
    RelayTransport::new(session_id.to_string(), outbound_tx)
}

/// relay 出站连接的 `Transport` 只是占位：真连接由 `relay_client` 用
/// `tokio_tungstenite` 建立（QQ / 企微网关已经在用同一个 crate，不引新依赖）。
/// 这里保留 `Arc` 出口是为了让 `handle_ws_stream` 的泛型参数在两条接入路径上
/// 形状一致。
pub type SharedRelayTransport = Arc<dyn Transport + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay_proto::{FrameAuth, PROTO_VERSION};
    use serde_json::json;

    fn env(sid: &str, payload: &str) -> Envelope {
        Envelope {
            session_id: Some(sid.to_string()),
            auth: Some(FrameAuth {
                bearer: "hmxd_x".into(),
                signature: "t=1,n=a,s=b".into(),
            }),
            payload: Some(payload.to_string()),
            ..Envelope::new(FrameTag::Route)
        }
    }

    #[tokio::test]
    async fn 多会话不串() {
        let mut mux = SessionMux::new();
        let (_a_tx, a_rx) = mux.pair_for("sid-a");
        let (_b_tx, b_rx) = mux.pair_for("sid-b");
        assert_eq!(mux.len(), 2);

        assert!(mux.dispatch(&env("sid-a", "给甲的消息")));
        assert!(mux.dispatch(&env("sid-b", "给乙的消息")));

        assert_eq!(a_rx.lock().await.recv().await.as_deref(), Some("给甲的消息"));
        assert_eq!(b_rx.lock().await.recv().await.as_deref(), Some("给乙的消息"));
    }

    #[tokio::test]
    async fn 缺session_id或payload一律丢() {
        let mut mux = SessionMux::new();
        let mut no_sid = env("x", "y");
        no_sid.session_id = None;
        assert!(!mux.dispatch(&no_sid), "缺 session_id 不得投给随便某条会话");

        let mut no_payload = env("sid-a", "y");
        no_payload.payload = None;
        assert!(!mux.dispatch(&no_payload));

        assert!(mux.is_empty(), "丢掉的帧不该凭空建出会话");
    }

    #[test]
    fn 非route帧不进会话() {
        let mut mux = SessionMux::new();
        let mut ack = Envelope::new(FrameTag::Ack);
        ack.session_id = Some("sid-a".into());
        assert!(!mux.dispatch(&ack), "ack/close 不是业务帧");
        assert!(mux.is_empty());
    }

    #[test]
    fn 会话结束后摘掉_重连不堆死队列() {
        let mut mux = SessionMux::new();
        let sink = mux.sink_for("sid-a");
        assert!(mux.contains("sid-a"));
        mux.remove("sid-a");
        assert!(!mux.contains("sid-a"));
        assert!(mux.is_empty());
        // 摘掉之后旧句柄失效，投递失败而不是悄悄进死队列
        assert!(sink.is_closed() || sink.try_send("x".into()).is_err());
    }

    #[tokio::test]
    async fn 出站帧带session_id_且不带凭据() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        // send/close 在 TransportSink 上，先拆出 sink —— 直接对 RelayTransport
        // 调 send 是编译不过的（它只实现 Transport）。
        let (mut sink, _src) = transport.split();
        sink.send(r#"{"type":"done"}"#.into()).await.unwrap();

        let got = rx.recv().await.expect("应有出站帧");
        assert_eq!(got.tag, FrameTag::Route);
        assert_eq!(got.session_id.as_deref(), Some("sid-a"));
        assert_eq!(got.auth, None, "出站方向不需要凭据");
        assert_eq!(got.proto_version, PROTO_VERSION);
        let payload: serde_json::Value =
            serde_json::from_str(got.payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload["type"], "turn.done", "payload 是应用侧事件，不是 hermix 那套");
    }

    #[tokio::test]
    async fn 非法json出站帧被拒而不是原样发出() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        let (mut sink, _src) = transport.split();
        // handler 只会发 JSON；非 JSON 说明有 bug，宁可断这一条也别把
        // 垃圾透传给 App（它会静默忽略，且现场毫无线索）
        assert!(sink.send("这不是 JSON".into()).await.is_err());
        assert!(rx.try_recv().is_err(), "坏帧不得发出");
    }

    #[tokio::test]
    async fn close只关这一条会话() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        let (mut sink, _src) = transport.split();
        sink.close().await.unwrap();
        let got = rx.recv().await.expect("应有 close 帧");
        assert_eq!(got.tag, FrameTag::Close);
        assert_eq!(got.session_id.as_deref(), Some("sid-a"));
    }

    /// **relay 边界就是两种协议的交界处**：handler 说 `ProtocolMessage`，
    /// 手机说 App 事件名。这条测试守着转换确实发生——没转换的话 App 收到的是
    /// 桌面端那套 type，UI 静默忽略，症状是"消息莫名消失"且毫无报错。
    #[tokio::test]
    async fn 出站帧已转成应用侧事件名() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        let (mut sink, _src) = transport.split();
        sink.send(r#"{"type":"tool_call","session_id":"sid-a","tool":"read_file"}"#.into())
            .await
            .unwrap();

        let got = rx.recv().await.expect("应有出站帧");
        let payload: serde_json::Value =
            serde_json::from_str(got.payload.as_deref().unwrap()).expect("payload 是 JSON");
        assert_eq!(payload["type"], "tool.start", "必须是 App 侧事件名");
        assert_eq!(payload["tool"], "read_file");
    }

    #[tokio::test]
    async fn 不可映射的帧不转发_而不是原样丢给应用() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        let (mut sink, _src) = transport.split();
        // 桌面端专属的内部事件，手机不需要
        sink.send(r#"{"type":"internal_zombie"}"#.into()).await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "给 App 一个它不认识的 type 会导致静默忽略，必须在边界上拦掉"
        );

        // 但可映射的要照发
        sink.send(r#"{"type":"done"}"#.into()).await.unwrap();
        let got = rx.recv().await.expect("可映射的帧应被转发");
        let payload: serde_json::Value =
            serde_json::from_str(got.payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload["type"], "turn.done");
    }

    #[tokio::test]
    async fn 审批帧带can_persist_false() {
        let (tx, mut rx) = mpsc::channel(8);
        let (transport, _inbound) = new_relay_transport("sid-a", tx);
        let (mut sink, _src) = transport.split();
        sink.send(
            r#"{"type":"request_approval","request_id":"r1","tool":"terminal"}"#.into(),
        )
        .await
        .unwrap();
        let got = rx.recv().await.expect("审批帧应转发");
        let payload: serde_json::Value =
            serde_json::from_str(got.payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload["type"], "approval.request");
        assert_eq!(
            payload["can_persist"],
            json!(false),
            "必须显式告诉 App 永久放行不可用，否则它只能静默少一个按钮"
        );
    }
}
