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

use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use tokio::sync::mpsc;

use crate::relay_crypto::{
    is_key_exchange, open_envelope, parse_key_exchange, seal_envelope, Direction,
};
use crate::relay_identity::PeerStore;
use crate::relay_proto::{Envelope, FrameTag};
use crate::transport::{Transport, TransportResult, TransportSink, TransportSource};

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
    /// E2EE 上下文。`None` = 未启用（测试路径），帧按明文走。
    e2ee: Option<Arc<E2eeCtx>>,
}

/// 进程级的 E2EE 状态：本机私钥 + 各设备的公钥。
///
/// 不是 per-session 的——私钥只有一份，设备公钥跨会话复用。`RelayTransport`
/// 与 `SessionMux` 共享同一个 `Arc`，这样手机发来的 `key.exchange` 无论在
/// 哪个会话被 dispatch，学到的公钥都立刻对所有出站可见。
pub struct E2eeCtx {
    own: SecretKey,
    /// device_id → 对方静态公钥。`key.exchange` 之前没有。
    peers: std::sync::Mutex<HashMap<String, PublicKey>>,
    /// `None` = 只在内存里记，不落盘。单测用——测试不该往工作区写文件。
    store: Option<PeerStore>,
}

impl E2eeCtx {
    /// 从磁盘身份建。已经配对过的设备公钥从 `peers.json` 读回来。
    pub fn load() -> Result<Self, crate::relay_identity::IdentityError> {
        let id = crate::relay_identity::load_or_create()?;
        let own = p256::SecretKey::from_slice(&id.secret_bytes())
            .map_err(|e| crate::relay_identity::IdentityError::Crypto(e.to_string()))?;
        let store = PeerStore::at_default();
        let mut peers = HashMap::new();
        for (device_id, pk) in read_all_peers() {
            peers.insert(device_id, pk);
        }
        Ok(Self {
            own,
            peers: std::sync::Mutex::new(peers),
            store: Some(store),
        })
    }

    pub fn own_public(&self) -> [u8; 65] {
        self.own
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .try_into()
            .expect("uncompressed P-256 point is always 65 bytes")
    }

    pub fn peer(&self, device_id: &str) -> Option<PublicKey> {
        self.peers.lock().ok()?.get(device_id).copied()
    }

    /// 记住一个设备公钥并落盘。
    fn learn_peer(&self, device_id: &str, pk: &PublicKey) {
        let raw: [u8; 65] = pk
            .to_encoded_point(false)
            .as_bytes()
            .try_into()
            .expect("uncompressed P-256 point is always 65 bytes");
        if let Ok(mut g) = self.peers.lock() {
            g.insert(device_id.to_string(), *pk);
        }
        let Some(store) = &self.store else { return };
        if let Err(e) = store.put(device_id, &raw) {
            tracing::warn!(device_id, error = %e, "设备公钥落盘失败（本次会话仍可用）");
        }
    }

    /// 拆开一帧入站密文。方向恒为手机 → PC。
    fn open_inbound(&self, env: &Envelope) -> Result<String, String> {
        open_envelope(env, &self.own, Direction::DeviceToNode).map_err(|e| e.to_string())
    }

    /// 封一帧出站明文。方向恒为 PC → 手机。
    fn seal_outbound(
        &self,
        env: &mut Envelope,
        plain: &str,
        device_id: &str,
    ) -> Result<(), String> {
        let peer = self
            .peer(device_id)
            .ok_or_else(|| format!("设备 {device_id} 尚未完成密钥交换，无法发送加密帧"))?;
        let frame_id = random_frame_id();
        seal_envelope(env, plain, &peer, &frame_id).map_err(|e| e.to_string())
    }
}

fn read_all_peers() -> Vec<(String, p256::PublicKey)> {
    // peers.json 的键就是 device_id；读不成就当没配对过，密钥交换会补上。
    let Ok(raw) = std::fs::read_to_string(
        hermix_config::paths::hermix_home().join("peers.json"),
    ) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(map) = v.get("peers").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    map.iter()
        .filter_map(|(id, b64s)| {
            let raw = b64.decode(b64s.as_str()?).ok()?;
            let arr: [u8; 65] = raw.try_into().ok()?;
            let pk = PublicKey::from_sec1_bytes(&arr).ok()?;
            Some((id.clone(), pk))
        })
        .collect()
}

/// 帧号：进 HKDF salt。两端不要求一致（对方从 `eid` 字段拿），只需**每帧唯一**——
/// 重复会削弱"跨帧重放"防护。16 字节随机。
fn random_frame_id() -> [u8; 16] {
    use rand::RngCore;
    let mut id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut id);
    id
}

/// `im:device:{device_id}` → `device_id`。解析不出来返回 `None`
/// （非设备来源的 sid 不参与 E2EE 密钥查找）。
pub fn device_id_of(session_id: &str) -> Option<&str> {
    session_id.strip_prefix("im:device:")
}

/// 一条会话的入站队列。上限是有意的：手机断线时不至于把 PC 的内存吃光。
const INBOUND_CAP: usize = 256;

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
        e2ee: Option<Arc<E2eeCtx>>,
    ) -> Self {
        let (inbound_tx, inbound_rx) = inbound;
        Self {
            session_id,
            inbound_tx,
            inbound_rx,
            outbound_tx,
            e2ee,
        }
    }

    fn new(session_id: String, outbound_tx: mpsc::Sender<Envelope>) -> (Self, mpsc::Sender<String>) {
        Self::new_with_e2ee(session_id, outbound_tx, None)
    }

    fn new_with_e2ee(
        session_id: String,
        outbound_tx: mpsc::Sender<Envelope>,
        e2ee: Option<Arc<E2eeCtx>>,
    ) -> (Self, mpsc::Sender<String>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAP);
        let rx = Arc::new(tokio::sync::Mutex::new(inbound_rx));
        (
            Self {
                session_id,
                inbound_tx: inbound_tx.clone(),
                inbound_rx: rx,
                outbound_tx,
                e2ee,
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
                e2ee: self.e2ee,
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
    e2ee: Option<Arc<E2eeCtx>>,
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
        let inner =
            serde_json::to_string(&mapped).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        // AAD 取路由字段，所以先把 Envelope 的路由部分填好，payload 留空
        // 占位——`seal_outbound` 会就地填上密文。
        let mut env = Envelope::route_for(&self.session_id, String::new());
        match &self.e2ee {
            None => {
                // 未启用 E2EE（单测 / 旧路径）：明文走。
                env.payload = Some(inner);
            }
            Some(ctx) => {
                // **没配对成功就发不出去**——这是有意的。发明文等于让 relay
                // 看穿，relay 侧也会直接拒（`isSealed`）。宁可这里明确失败，
                // 也不要一条"看起来成功但其实裸奔"的路径。
                let device_id = device_id_of(&self.session_id)
                    .ok_or("session_id 不是设备来源，无从取对端公钥")?;
                ctx.seal_outbound(&mut env, &inner, device_id)?;
            }
        }
        self.outbound_tx
            .send(env)
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
    /// E2EE 上下文，与 `transport_for` 建出的 `RelayTransport` 共享同一个
    /// `Arc`——手机在任何会话发来的 `key.exchange`，学到的公钥都立刻对
    /// 所有出站可见。`None` = 未启用（单测路径），帧按明文走。
    e2ee: Option<Arc<E2eeCtx>>,
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

    /// 启用 E2EE：从磁盘身份加载，并读回已配对设备的公钥。
    ///
    /// 加载失败（DPAPI 拒绝 / 文件损坏）**不静默降级成明文**——那等于
    /// 用户以为在加密、实际裸奔。调用方必须自己决定是报错还是提示。
    pub fn with_e2ee() -> Result<Self, crate::relay_identity::IdentityError> {
        Ok(Self {
            entries: HashMap::new(),
            e2ee: Some(Arc::new(E2eeCtx::load()?)),
        })
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
        RelayTransport::over_channel(sid.to_string(), outbound_tx, inbound, self.e2ee.clone())
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
    ///
    /// 启用 E2EE 时这里还负责解密。`key.exchange` 帧**不投给上层**——它是
    /// 配对协议的一部分，不是业务消息；学完公钥就地返回。
    pub fn dispatch(&mut self, env: &Envelope) -> bool {
        if env.tag != FrameTag::Route {
            return false;
        }
        let (Some(sid), Some(payload)) = (env.session_id.as_deref(), env.payload.as_deref()) else {
            tracing::warn!("relay 帧缺 session_id 或 payload，已丢弃");
            return false;
        };

        let plain = match &self.e2ee {
            None => payload.to_string(),
            Some(ctx) => match ctx.open_inbound(env) {
                Ok(p) => p,
                Err(e) => {
                    // 解不开 = AAD/salt/方向/密钥有一个对不上。**不投**——
                    // 投给上层等于让它处理一段自己没验证过的字节。
                    tracing::warn!(session = %sid, error = %e, "入站帧解密失败，已丢弃");
                    return false;
                }
            },
        };

        // 密钥交换：学到设备公钥，**不投给上层**（它不是业务消息）。
        if is_key_exchange(&plain) {
            let Some(ctx) = &self.e2ee else {
                tracing::warn!("未启用 E2EE 却收到 key.exchange 帧，已丢弃");
                return false;
            };
            match parse_key_exchange(&plain) {
                Ok((device_id, pk)) => {
                    let parsed = PublicKey::from_sec1_bytes(&pk);
                    match parsed {
                        Ok(pk) => {
                            tracing::info!(
                                device_id = %device_id,
                                "已学到设备公钥，E2EE 出站就绪"
                            );
                            ctx.learn_peer(&device_id, &pk);
                        }
                        Err(e) => {
                            tracing::warn!(device_id = %device_id, error = %e, "设备公钥不是合法 P-256 点，已丢弃");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(session = %sid, error = %e, "key.exchange 帧不合法，已丢弃");
                }
            }
            return true;
        }

        // 会话不在表里 = 还没起（首帧竞态）→ 建一个。已结束的那次不会再有帧。
        let sink = self.sink_for(sid);
        // spawn 的 `async move` 会把捕获物带出本方法，而 `sid` / `payload`
        // 都是从 `env` 借来的 `&str`——跨 await 的 future 不能带借用
        // （E0521）。所以先把两者转成自有字符串。
        let sid_owned = sid.to_string();
        let payload_owned = plain;
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
    use crate::relay_crypto::{build_key_exchange_envelope, is_sealed_payload, open_envelope, Direction};

    fn e2ee_for_tests(own_seed: u8) -> Arc<E2eeCtx> {
        Arc::new(E2eeCtx {
            own: p256::SecretKey::from_slice(&[own_seed; 32]).unwrap(),
            peers: std::sync::Mutex::new(HashMap::new()),
            store: None,
        })
    }

    // ── E2EE 接线 ────────────────────────────────────────────────

    /// PC 用手机公钥封 → 手机用自己的私钥解（模拟手机侧）。
    #[test]
    fn e2ee_seal_open_roundtrip() {
        let ctx = e2ee_for_tests(1);
        let phone = p256::SecretKey::from_slice(&[2u8; 32]).unwrap();
        ctx.peers
            .lock()
            .unwrap()
            .insert("dev-1".to_string(), phone.public_key());

        let mut env = Envelope::route_for("im:device:dev-1", String::new());
        ctx.seal_outbound(&mut env, "机密内容", "dev-1").expect("seal");

        let payload = env.payload.as_deref().unwrap();
        assert!(is_sealed_payload(payload), "出站必须是密封信封");
        assert!(!payload.contains("机密"), "明文不得留在信封里");
        // relay 看得见信封但读不到内容；手机用自己的私钥能解。
        assert_eq!(
            open_envelope(&env, &phone, Direction::NodeToDevice).expect("open"),
            "机密内容"
        );
    }

    /// 没配对成功就发不出去——不能发明文，宁可明确失败。
    #[test]
    fn e2ee_outbound_refuses_without_peer_key() {
        let ctx = e2ee_for_tests(1);
        let mut env = Envelope::route_for("im:device:dev-x", String::new());
        assert!(ctx.seal_outbound(&mut env, "x", "dev-x").is_err());
    }

    /// 明文 route 帧在启用 E2EE 时被拒——不静默放行。
    #[test]
    fn e2ee_rejects_plaintext_inbound() {
        let ctx = e2ee_for_tests(1);
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(ctx),
        };
        let plain = Envelope {
            session_id: Some("im:device:dev-1".into()),
            payload: Some(r#"{"type":"chat"}"#.into()),
            ..Envelope::new(FrameTag::Route)
        };
        assert!(!mux.dispatch(&plain), "明文帧必须被拒");
    }

    /// `key.exchange` 学到公钥后**出站立刻可用**，且该帧不投给上层。
    #[test]
    fn key_exchange_arms_e2ee_and_stays_out_of_the_queue() {
        let pc = e2ee_for_tests(9);
        let phone = p256::SecretKey::from_slice(&[8u8; 32]).unwrap();
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(pc.clone()),
        };

        // 手机用 PC 公钥封密钥交换。
        let mut env = Envelope::route_for("im:device:dev-9", String::new());
        build_key_exchange_envelope(
            &mut env,
            &pc.own.public_key(),
            "dev-9",
            &phone
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .try_into()
                .unwrap(),
            b"kx",
        )
        .expect("build");

        assert!(mux.dispatch(&env), "key.exchange 应被接受");
        assert!(!mux.contains("im:device:dev-9"), "key.exchange 不该建会话队列");
        assert!(pc.peer("dev-9").is_some(), "公钥应已学到");

        // 学到之后出站立刻可用。
        let mut out = Envelope::route_for("im:device:dev-9", String::new());
        assert!(pc.seal_outbound(&mut out, "hi", "dev-9").is_ok());
    }

    /// 未启用 E2EE 时保持原行为（单测走的明文路径不被这次改动打断）。
    #[tokio::test]
    async fn plaintext_still_works_without_e2ee() {
        let mut mux = SessionMux::new();
        let (_tx, rx) = mux.pair_for("im:device:dev-1");
        assert!(mux.dispatch(&env("im:device:dev-1", "明文帧")));
        assert_eq!(rx.lock().await.recv().await.as_deref(), Some("明文帧"));
    }
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
    async fn 多会话不串() {        let mut mux = SessionMux::new();
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
