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
    /// 会话事件日志。`None` = 不做补发（单测路径）。
    ///
    /// 生命周期必须长于 relay 连接——放在 `SessionMux` 里就会随每次重连一起
    /// 丢掉，而「重连后补齐」正是它存在的理由。
    journal: Option<Arc<crate::session_journal::JournalRegistry>>,
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

/// 封一帧出站内层明文并发到 relay。
///
/// `RelaySink::send` 与补发回信共用——**没配对成功就发不出去**这条规则
/// 只能有一处实现，两边各写一遍必然漂移。宁可不发，也不发明文。
async fn seal_and_send(
    outbound_tx: &mpsc::Sender<Envelope>,
    e2ee: &Option<Arc<E2eeCtx>>,
    session_id: &str,
    inner: String,
) -> Result<(), String> {
    // AAD 取路由字段，所以先把 Envelope 的路由部分填好，payload 留空
    // 占位——`seal_outbound` 会就地填上密文。
    let mut env = Envelope::route_for(session_id, String::new());
    match e2ee {
        None => env.payload = Some(inner),
        Some(ctx) => {
            let device_id =
                device_id_of(session_id).ok_or("session_id 不是设备来源，无从取对端公钥")?;
            ctx.seal_outbound(&mut env, &inner, device_id)?;
        }
    }
    outbound_tx.send(env).await.map_err(|e| e.to_string())
}

/// 一条会话的入站队列。上限是有意的：手机断线时不至于把 PC 的内存吃光。
const INBOUND_CAP: usize = 256;

impl RelayTransport {
    /// 用**已有**的入站通道构造一条 Transport。
    ///
    /// 必须与 `SessionMux` 持有的是同一条通道——各自建一条的话，投进 mux
    /// 的帧永远到不了 transport，而这种错误不报任何错（编译过、测试也过），
    /// 症状是「手机发的消息 agent 收不到」。
    /// 带事件日志的版本：`journal = None` 时不做补发（单测路径）。
    pub fn over_channel_with_journal(
        session_id: String,
        outbound_tx: mpsc::Sender<Envelope>,
        inbound: Inbound,
        e2ee: Option<Arc<E2eeCtx>>,
        journal: Option<Arc<crate::session_journal::JournalRegistry>>,
    ) -> Self {
        let (inbound_tx, inbound_rx) = inbound;
        Self {
            session_id,
            inbound_tx,
            inbound_rx,
            outbound_tx,
            e2ee,
            journal,
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
                journal: None,
            },
            inbound_tx,
        )
    }
}

impl Transport for RelayTransport {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>) {
        let RelayTransport {
            session_id,
            inbound_tx,
            inbound_rx,
            outbound_tx,
            e2ee,
            journal,
        } = self;
        (
            Box::new(RelaySink {
                session_id: session_id.clone(),
                outbound_tx: outbound_tx.clone(),
                e2ee: e2ee.clone(),
                journal: journal.clone(),
            }),
            // 补发走 recv 这条路：手机发来的 `sync` 请求必须在交给
            // `handle_ws_stream` 之前截下来（它只认 `WsClientMessage`，
            // 见到不认识的 type 会整帧丢弃且不报错）。
            Box::new(RelaySource {
                session_id,
                inbound_rx,
                inbound_tx,
                outbound_tx,
                e2ee,
                journal,
            }),
        )
    }
}

struct RelaySink {
    session_id: String,
    outbound_tx: mpsc::Sender<Envelope>,
    e2ee: Option<Arc<E2eeCtx>>,
    journal: Option<Arc<crate::session_journal::JournalRegistry>>,
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
        // 序号必须在**帧里**：手机用它记「我看到哪了」，重连时把 last_seq
        // 带回来。日志存的也必须是含 seq 的同一份字节——否则补发出去的帧
        // 没有序号，手机会一直认为自己停在原地。
        let inner = match &self.journal {
            Some(journal) => journal
                .append(&self.session_id, |seq| {
                    mapped["seq"] = serde_json::Value::from(seq);
                    mapped.to_string()
                })
                .1,
            None => serde_json::to_string(&mapped)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?,
        };

        // 出站方向只走这一条封帧路径（与补发回信共用）。
        seal_and_send(&self.outbound_tx, &self.e2ee, &self.session_id, inner)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
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
    session_id: String,
    inbound_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
    /// 保留发送端：否则 `handle_ws_stream` 一放开 Recv 就会看到通道关闭，
    /// 把"暂时没有新帧"误判成"对端已关闭"。
    inbound_tx: mpsc::Sender<String>,
    /// 补发要自己封帧发回去，所以接收侧也得握有出站口与 E2EE 上下文。
    outbound_tx: mpsc::Sender<Envelope>,
    e2ee: Option<Arc<E2eeCtx>>,
    journal: Option<Arc<crate::session_journal::JournalRegistry>>,
}

#[async_trait::async_trait]
impl TransportSource for RelaySource {
    async fn recv(&mut self) -> Option<String> {
        // 发送端由本结构体持有（见字段注释），所以它会一直 await 到真有帧，
        // 而不是把"暂时没消息"当成"对端已关闭"。
        loop {
            let msg = { self.inbound_rx.lock().await.recv().await }?;
            // 补发请求在这里截住：往下走是 `handle_ws_stream`，它只认业务帧，
            // 见到 `sync` 会整帧丢弃且**不报任何错**——症状是「手机重连后
            // 一片空白，日志里什么都没有」。
            match crate::mobile_protocol::parse_sync_request(&msg) {
                Some(since_seq) => self.reply_sync(since_seq).await,
                None => return Some(msg),
            }
        }
    }
}

impl RelaySource {
    /// 按 `since_seq` 把漏掉的事件打包回信。
    async fn reply_sync(&self, since_seq: u64) {
        let Some(journal) = &self.journal else {
            tracing::warn!(session = %self.session_id, "收到补发请求但没有事件日志，忽略");
            return;
        };
        let (events, truncated) = journal.since(&self.session_id, since_seq);
        let frames: Vec<serde_json::Value> = events
            .iter()
            .filter_map(|(seq, raw)| match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(seq, error = %e, "日志里的帧不是合法 JSON，跳过");
                    None
                }
            })
            .collect();
        tracing::info!(
            session = %self.session_id,
            since_seq,
            count = frames.len(),
            truncated,
            "补发断连期间的事件"
        );
        let payload = serde_json::json!({
            "type": crate::mobile_protocol::MOBILE_SYNC_RESULT,
            "proto_version": crate::relay_proto::PROTO_VERSION,
            "since_seq": since_seq,
            // 缺口必须**明说**：环形日志覆盖掉的那一段真的没了，
            // 让 UI 提示「部分历史已丢失」，而不是给一份看起来完整的列表。
            "truncated": truncated,
            "events": frames,
        });
        if let Err(e) = seal_and_send(
            &self.outbound_tx,
            &self.e2ee,
            &self.session_id,
            payload.to_string(),
        )
        .await
        {
            tracing::warn!(session = %self.session_id, error = %e, "补发回信发送失败");
        }
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
    /// 入站帧号去重（见 [`ReplayGuard`]）。
    replay: ReplayGuard,
}

/// 从密封信封里取 16 字节帧号（非密封帧 → None）。
fn frame_id_of(env: &Envelope) -> Option<[u8; 16]> {
    use base64::Engine;
    let payload = env.payload.as_deref()?;
    let frame: crate::relay_proto::SealedFrame = serde_json::from_str(payload).ok()?;
    base64::engine::general_purpose::STANDARD
        .decode(frame.eid)
        .ok()?
        .try_into()
        .ok()
}

/// 入站帧重放去重。
///
/// **为什么需要**：`eid` 每帧随机、只进 HKDF salt——它保证每帧密钥唯一，
/// 但**密文原样重放照样能正常解密**（nonce/eph/AAD 全部一致）。托管形态的
/// 威胁模型里 relay 运营方是潜在攻击者：把捕获的旧"执行命令"帧重投一遍，
/// 桌面端会当作新指令再执行。签名的 ±300s 新鲜度把窗口限住，但窗口内
/// 仍可重放——这一层就是补那段窗口。
///
/// 按 node 分桶，600s TTL（覆盖签名窗口）+ 每桶 256 上限双限内存；
/// **只在解密成功后登记**，攻击者无法用垃圾 eid 投毒桶。
#[derive(Default)]
pub struct ReplayGuard {
    buckets: HashMap<String, std::collections::VecDeque<(std::time::Instant, [u8; 16])>>,
}

impl ReplayGuard {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(600);
    const MAX_PER_NODE: usize = 256;

    /// 窗口内出现过 → true（重放，调用方应丢弃）。
    pub fn is_replay(&mut self, node: &str, eid: [u8; 16]) -> bool {
        let now = std::time::Instant::now();
        let q = self.buckets.entry(node.to_string()).or_default();
        while let Some((ts, _)) = q.front() {
            if now.duration_since(*ts) > Self::WINDOW {
                q.pop_front();
            } else {
                break;
            }
        }
        if q.iter().any(|(_, e)| *e == eid) {
            return true;
        }
        if q.len() < Self::MAX_PER_NODE {
            q.push_back((now, eid));
        }
        false
    }
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
            replay: ReplayGuard::default(),
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
        journal: Option<Arc<crate::session_journal::JournalRegistry>>,
    ) -> RelayTransport {
        let inbound = self
            .entries
            .entry(sid.to_string())
            .or_insert_with(new_inbound)
            .clone();
        RelayTransport::over_channel_with_journal(
            sid.to_string(),
            outbound_tx,
            inbound,
            self.e2ee.clone(),
            journal,
        )
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
            // 无 E2EE 上下文时**绝不放行密封帧**：透传等于让上层处理一段
            // 解不开的密文——历史上就是这么把 `{ct:…}` 喂给 LLM、还把密文
            // 当会话历史落了盘（2026-10-16 案）。明文帧照旧放行（单测路径）。
            None if crate::relay_crypto::is_sealed_payload(payload) => {
                tracing::warn!(
                    session = %sid,
                    "未启用 E2EE，密封帧已拒收（绝不透传密文给上层）"
                );
                return false;
            }
            None => payload.to_string(),
            Some(ctx) => match ctx.open_inbound(env) {
                Ok(p) => {
                    // 解密成功后查重：同 eid 的密文原样重投能正常解密，
                    // 不去重就是一次静默的指令重放（ReplayGuard 文档）。
                    if let Some(eid) = frame_id_of(env) {
                        let node = env.node_id.as_deref().unwrap_or("");
                        if self.replay.is_replay(node, eid) {
                            tracing::warn!(session = %sid, node = %node, "eid 重放帧，已丢弃");
                            return false;
                        }
                    }
                    p
                }
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
                    // 明文里自报的 `device_id` 必须与信封路由一致。
                    //
                    // 少了这一步，已配对设备 A 可以在自己会话里声明
                    // `device_id = B`，把 B 的公钥条目覆盖成自己的——此后 PC 发给
                    // B 的帧 A 能解开。B 下次 key.exchange 会自愈，但自愈之前的
                    // 窗口是白送的。`device_id_of` 本来就是这条会话的可信来源。
                    match device_id_of(sid) {
                        Some(sid_device) if sid_device == device_id => {}
                        other => {
                            tracing::warn!(
                                session = %sid,
                                claimed = %device_id,
                                routed = ?other,
                                "key.exchange 声明的 device_id 与信封路由不符，已丢弃"
                            );
                            return false;
                        }
                    }
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

    /// 回归守卫（2026-10-16 密文案）：**未启用 E2EE 时密封帧必须被拒**。
    /// 放行的后果不是「看不见」，而是 LLM 对着 `{ct:…}` 照常胡说、并把密文
    /// 当会话历史落库——现场完全看不出是安全故障，只像模型变傻了。
    #[test]
    fn no_e2ee_rejects_sealed_inbound_never_transits_ciphertext() {
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: None,
            replay: ReplayGuard::default(),
        };
        let sealed = Envelope {
            session_id: Some("im:device:dev-1".into()),
            payload: Some(
                r#"{"ct":"BAtz","nonce":"AAAA","ep":"BAtz","eid":"BAtz"}"#.into(),
            ),
            ..Envelope::new(FrameTag::Route)
        };
        assert!(!mux.dispatch(&sealed), "无 E2EE 时密封帧必须被拒");
        assert!(!mux.contains("im:device:dev-1"), "被拒的帧不该建会话队列");
    }

    /// 明文 route 帧在启用 E2EE 时被拒——不静默放行。
    #[test]
    fn e2ee_rejects_plaintext_inbound() {
        let ctx = e2ee_for_tests(1);
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(ctx),
            replay: ReplayGuard::default(),
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
            replay: ReplayGuard::default(),
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

    /// 已配对设备 A 不能在自己会话里声明 `device_id = B` 去覆盖 B 的公钥。
    ///
    /// 没有这道校验时，A 学到的公钥会顶掉 B 的条目，此后 PC 发给 B 的帧
    /// A 能解开；要等 B 下一次 `key.exchange` 才自愈，中间的窗口是白送的。
    #[test]
    fn key_exchange_cannot_claim_another_device_id() {
        let pc = e2ee_for_tests(9);
        let attacker = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(pc.clone()),
            replay: ReplayGuard::default(),
        };

        // 路由到 dev-9 的会话，却在明文里自称 dev-victim。
        let mut env = Envelope::route_for("im:device:dev-9", String::new());
        build_key_exchange_envelope(
            &mut env,
            &pc.own.public_key(),
            "dev-victim",
            &attacker
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .try_into()
                .unwrap(),
            b"kx",
        )
        .expect("build");

        assert!(!mux.dispatch(&env), "device_id 与路由不符的帧必须拒收");
        assert!(
            pc.peer("dev-victim").is_none(),
            "不得因此学到/覆盖别的设备公钥"
        );
        assert!(pc.peer("dev-9").is_none(), "也不该把攻击者公钥记到 dev-9");
    }

    /// 非设备来源的会话（sid 不带 `im:device:` 前缀）不该接受 key.exchange：
    /// 没有可信的两端身份可比对，学了就是无条件信任。
    #[test]
    fn key_exchange_rejected_for_non_device_session() {
        let pc = e2ee_for_tests(9);
        let phone = p256::SecretKey::from_slice(&[8u8; 32]).unwrap();
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(pc.clone()),
            replay: ReplayGuard::default(),
        };
        let mut env = Envelope::route_for("stream:local-1", String::new());
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

        assert!(!mux.dispatch(&env));
        assert!(pc.peer("dev-9").is_none());
    }

    // ── 入站重放去重（ReplayGuard） ───────────────────────────────
    /// 用手机身份封一帧"投给 PC"的入站帧。方向必须显式给 DeviceToNode
    /// ——`seal_envelope` 写死 NodeToDevice（PC 出站专用），所以这里走
    /// 低层 `seal_body` + 手工组装信封，与生产入站方向一致。
    fn sealed_inbound(
        ctx: &E2eeCtx,
        sid: &str,
        node_id: &str,
        plain: &str,
        eid: &[u8; 16],
    ) -> Envelope {
        use base64::Engine;
        let mut env = Envelope::route_for(sid, String::new());
        env.node_id = Some(node_id.to_string());
        let fctx = crate::relay_crypto::FrameContext {
            frame_id: eid,
            header: &env.aad(),
            direction: Direction::DeviceToNode,
        };
        let sealed = crate::relay_crypto::seal_body(plain.as_bytes(), &ctx.own.public_key(), &fctx);
        let b64 = base64::engine::general_purpose::STANDARD;
        env.payload = Some(
            serde_json::json!({
                "ct": b64.encode(&sealed.ciphertext),
                "nonce": b64.encode(sealed.nonce),
                "ep": b64.encode(sealed.ephemeral_public),
                "eid": b64.encode(eid),
            })
            .to_string(),
        );
        env
    }

    /// **守 `relay_client` 的入站契约**：密文信封投进来，队列里必须是
    /// **解密后的明文**。生产路径（`relay_client` 的 Route 分支）把
    /// `env.payload` 原样塞进同一个队列——若那条路不解密，会话收到的是
    /// `{"ct":…}`，`handle_ws_stream` 解析失败后静默丢弃，症状就是
    /// 「手机发消息电脑毫无反应」，且**不会报任何错**。
    #[tokio::test]
    async fn 密文入站解密后明文进队列() {
        let ctx = e2ee_for_tests(1);
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(ctx.clone()),
            replay: ReplayGuard::default(),
        };
        let (_tx, rx) = mux.pair_for("im:device:dev-1");
        let env = sealed_inbound(
            &ctx,
            "im:device:dev-1",
            "dev-1",
            r#"{"type":"send","text":"你好"}"#,
            &[9u8; 16],
        );
        assert!(mux.dispatch(&env), "合法密文帧应被接受");
        assert_eq!(
            rx.lock().await.recv().await.as_deref(),
            Some(r#"{"type":"send","text":"你好"}"#),
            "队列里必须是 handle_ws_stream 认得的明文，而不是密文信封"
        );
    }

    /// 同一密文原样重投：第一次投出，第二次被去重丢弃。
    /// eid 只进 HKDF salt——不去重的话这两次都会正常解密。
    #[test]
    fn replayed_sealed_frame_is_dropped_on_second_delivery() {
        let ctx = e2ee_for_tests(1);
        let phone = p256::SecretKey::from_slice(&[2u8; 32]).unwrap();
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(ctx.clone()),
            replay: ReplayGuard::default(),
        };
        let _ = &phone; // 方向由封帧方显式声明，私钥不参与构造
        let env = sealed_inbound(
            &ctx,
            "im:device:dev-1",
            "dev-1",
            r#"{"type":"chat","text":"rm -rf"}"#,
            &[7u8; 16],
        );
        assert!(mux.dispatch(&env), "首次投递应成功");
        assert!(!mux.dispatch(&env), "同一 eid 重放必须被丢弃");
    }

    /// 不同帧号的两帧都应正常通过（去重不能误伤）。
    #[test]
    fn distinct_frames_both_delivered() {
        let ctx = e2ee_for_tests(1);
        let phone = p256::SecretKey::from_slice(&[2u8; 32]).unwrap();
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(ctx.clone()),
            replay: ReplayGuard::default(),
        };
        let _ = &phone;
        let a = sealed_inbound(&ctx, "s", "dev-1", r#"{"n":1}"#, &[1u8; 16]);
        let b = sealed_inbound(&ctx, "s", "dev-1", r#"{"n":2}"#, &[2u8; 16]);
        assert!(mux.dispatch(&a));
        assert!(mux.dispatch(&b));
    }

    /// 桶行为：重复命中、不同帧号不命中、容量上限后停止登记（不无界增长）。
    #[test]
    fn replay_guard_dedups_within_window_and_caps_memory() {
        let mut g = ReplayGuard::default();
        assert!(!g.is_replay("dev-1", [1u8; 16]), "首次不是重放");
        assert!(g.is_replay("dev-1", [1u8; 16]), "同帧号再次出现是重放");
        assert!(!g.is_replay("dev-1", [2u8; 16]), "不同帧号不是重放");
        assert!(!g.is_replay("dev-2", [1u8; 16]), "分桶隔离：别的 node 独立");
        // 灌满上限后不再登记新帧号，但历史帧号仍能识别为重放。
        for i in 0..(ReplayGuard::MAX_PER_NODE as u16 + 10) {
            let _ = g.is_replay("dev-3", [i as u8; 16]);
        }
        assert!(g.is_replay("dev-3", [0u8; 16]), "桶内早期帧号仍识别为重放");
        assert_eq!(g.buckets.get("dev-3").map(|q| q.len()), Some(ReplayGuard::MAX_PER_NODE));
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

    // ── 断线重连补发（SessionJournal + sync） ────────────────────────

    /// 建一条带日志的会话，返回 (mux, 出站接收端)。「重连」= 再调一次：
    /// 真实重连就是 `SessionMux` 被丢掉重建，而日志活在它之外——**这正是
    /// 要验的不变量**，用重建 mux 来模拟比开真 socket 更稳且更贴题。
    fn wired_session(
        pc: &Arc<E2eeCtx>,
        sid: &str,
        journal: &Arc<crate::session_journal::JournalRegistry>,
    ) -> (
        SessionMux,
        RelayTransport,
        mpsc::Receiver<Envelope>,
    ) {
        let (out_tx, out_rx) = mpsc::channel(16);
        let mut mux = SessionMux {
            entries: HashMap::new(),
            e2ee: Some(pc.clone()),
            replay: ReplayGuard::default(),
        };
        let transport = mux.transport_for(sid, out_tx, Some(journal.clone()));
        (mux, transport, out_rx)
    }

    /// 手机断线期间的出站事件，重连后必须补齐。
    ///
    /// 验收整条链：`sink.send` → 事件名映射 → 记日志（带 seq）→ 封帧 →
    /// relay；重连时手机发 `sync{since_seq}` → `RelaySource::recv` 截获 →
    /// 打包成一条 `sync.result` 回信。
    #[tokio::test]
    async fn reconnect_replays_events_missed_while_offline() {
        let pc = e2ee_for_tests(9);
        let phone = p256::SecretKey::from_slice(&[8u8; 32]).unwrap();
        let sid = "im:device:dev-replay";
        let journal = Arc::new(crate::session_journal::JournalRegistry::new());
        // 生产上这条公钥是重连时那次 key.exchange 学到的；这里直接塞进去，
        // 免得测试同时验两件事。
        pc.peers
            .lock()
            .unwrap()
            .insert("dev-replay".to_string(), phone.public_key());

        // ---- 第一次连接：PC 发了 3 条，手机只收到前 2 条就断线了 ----
        let (_mux1, transport1, mut out_rx1) = wired_session(&pc, sid, &journal);
        let (mut sink1, _src1) = transport1.split();
        for text in ["one", "two", "three"] {
            sink1
                .send(json!({"type": "text", "content": text}).to_string())
                .await
                .expect("出站应成功");
        }

        let mut seen_last_seq = 0;
        for _ in 0..2 {
            let env = out_rx1.recv().await.expect("应有出站帧");
            let plain = open_envelope(&env, &phone, Direction::NodeToDevice).expect("手机应能解开");
            let v: serde_json::Value = serde_json::from_str(&plain).unwrap();
            assert_eq!(v["type"], "assistant.message");
            assert_eq!(v["proto_version"], crate::relay_proto::PROTO_VERSION);
            // 序号必须落在帧里，否则手机无从记「我看到哪了」。
            seen_last_seq = v["seq"].as_u64().expect("每帧都要带 seq");
        }
        assert_eq!(seen_last_seq, 2, "手机看到第 2 条就断了");

        // 第三条已经在 relay 的路由表里发不出去 —— 相当于断线期间产生，
        // 手机没收到的它，但日志里有。
        assert_eq!(journal.len(sid), 3, "3 条都要进日志，包括手机没收到的那条");

        // ---- 重连：新 mux（模拟 mux 重建），同一份日志 ----
        let (mut mux2, transport2, mut out_rx2) = wired_session(&pc, sid, &journal);
        let (_sink2, mut src2) = transport2.split();

        // 手机把 last_seq 带回来。
        mux2.sink_for(sid)
            .send(json!({"type": "sync", "since_seq": seen_last_seq}).to_string())
            .await
            .expect("sync 请求应入队");

        // recv 会在等业务帧的过程中把补发回信发出去。
        let recv_task = tokio::spawn(async move {
            // 没有后续业务帧，它会一直等——回信已经发出，abort 收尾。
            let _ = src2.recv().await;
        });

        let env = tokio::time::timeout(std::time::Duration::from_secs(2), out_rx2.recv())
            .await
            .expect("超时：补发回信没发出来")
            .expect("出站通道不该关闭");
        recv_task.abort();

        let plain = open_envelope(&env, &phone, Direction::NodeToDevice).expect("手机应能解开回信");
        let v: serde_json::Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(v["type"], "sync.result");
        assert_eq!(v["since_seq"], 2);
        assert_eq!(v["truncated"], json!(false), "没被环形日志覆盖就不该报缺口");
        let events = v["events"].as_array().expect("events 必须是数组");
        assert_eq!(events.len(), 1, "只补第 3 条");
        assert_eq!(events[0]["content"], "three");
        assert_eq!(events[0]["seq"], 3, "补发的帧要带原序号，手机才能续上进度");
    }

    /// 环形日志被覆盖时，回信必须**明说**缺口，而不是给一份看起来完整的列表。
    #[tokio::test]
    async fn replay_reports_truncation_instead_of_pretending_complete() {
        let pc = e2ee_for_tests(9);
        let phone = p256::SecretKey::from_slice(&[9u8; 32]).unwrap();
        let sid = "im:device:dev-trunc";
        // 容量 1：发 3 条只留最后一条。
        let journal = Arc::new(crate::session_journal::JournalRegistry::with_cap(1));
        pc.peers
            .lock()
            .unwrap()
            .insert("dev-trunc".to_string(), phone.public_key());

        let (_mux1, transport1, _out_rx1) = wired_session(&pc, sid, &journal);
        let (mut sink1, _src1) = transport1.split();
        for text in ["one", "two", "three"] {
            sink1
                .send(json!({"type": "text", "content": text}).to_string())
                .await
                .unwrap();
        }

        let (mut mux2, transport2, mut out_rx2) = wired_session(&pc, sid, &journal);
        let (_sink2, mut src2) = transport2.split();
        mux2.sink_for(sid)
            .send(json!({"type": "sync", "since_seq": 0}).to_string())
            .await
            .unwrap();
        let recv_task = tokio::spawn(async move {
            let _ = src2.recv().await;
        });

        let env = tokio::time::timeout(std::time::Duration::from_secs(2), out_rx2.recv())
            .await
            .expect("超时")
            .expect("通道关闭");
        recv_task.abort();

        let plain = open_envelope(&env, &phone, Direction::NodeToDevice).unwrap();
        let v: serde_json::Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(v["truncated"], json!(true), "被覆盖的区间必须报缺口");
        assert_eq!(v["events"].as_array().unwrap().len(), 1);
    }

    /// `sync` 不能被当成业务帧交给 `handle_ws_stream` —— 它按
    /// `WsClientMessage` 解析，遇到不认识的 type 会整帧丢弃且不报错。
    #[tokio::test]
    async fn sync_request_is_not_delivered_to_the_session_handler() {
        let pc = e2ee_for_tests(9);
        let sid = "im:device:dev-sync-hidden";
        let journal = Arc::new(crate::session_journal::JournalRegistry::new());
        let (mut mux, transport, _out_rx) = wired_session(&pc, sid, &journal);
        let (_sink, mut src) = transport.split();

        let tx = mux.sink_for(sid);
        tx.send(json!({"type": "sync", "since_seq": 0}).to_string())
            .await
            .unwrap();
        tx.send(json!({"type": "send", "text": "hi"}).to_string())
            .await
            .unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), src.recv())
            .await
            .expect("超时")
            .expect("应有业务帧");
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["type"], "send", "sync 被吃掉后才轮到真正的业务帧");
        assert_eq!(v["text"], "hi");
    }
}
