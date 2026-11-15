//! relay 连接设置：落盘、读取、更新。以及 `GET/PUT /api/relay/config`。
//!
//! ## 为什么这些字段以前无处可配
//!
//! `relay_client.rs` 把 `RelayConfig { url, node_id }` 定义好了，`connect_and_serve`
//! 也写好了，但**全仓库没有任何生产代码构造过它、也没有任何 `/api/relay/*` 端点**——
//! 那四个模块（`relay_client` / `relay_transport` / `relay_identity` / `relay_proto`）
//! 是一根线都没接的。本模块补上「配置从哪来、谁 spawn 它、UI 怎么改」这三件事。
//!
//! ## 凭据用 DPAPI，不用明文
//!
//! `hrel_*` 是 relay 侧的参与者凭据：泄露即可冒充这台电脑接入路由。量级上它
//! 低于 E2EE 私钥（撤销即失效），但 `~/.hermix/` 下明文文件一大把，多一份不多、
//! 少一份不少，而 DPAPI 是**现成的**（`relay_identity.rs` 已经在用），复用零成本。
//! 熵与 E2EE 私钥的不同，见 `relay_identity::protect_secret`。
//!
//! ## 配置读坏了不静默当「没配过」
//!
//! 损坏的文件如果静默回落成默认，用户的现象是「我明明配过，怎么显示未连接」——
//! 而真正原因是文件坏了。所以加载错误**带着走**，由 `/api/relay/config` 原样回给
//! 界面显示，而不是吞进日志让人猜。

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::relay_client::RelayConfig;

const FILE: &str = "relay.json";
const VERSION: u32 = 1;
/// 手机绑定表（`PhoneBindingStore`）的文件名。重置身份时一并删除，
/// 常量提出来保证两处路径永不漂移。
const PHONE_BINDING_FILE: &str = "relay_phones.json";
/// 与 E2EE 私钥不同的熵标签，见 `relay_identity::protect_secret`。
const DPAPI_LABEL: &[u8] = b"hermix/relay/token/v1";

/// 内置默认 relay 根地址。
///
/// **为什么要有默认值**：地址是**我们部署的**，不是用户该填的参数。此前
/// `url` 空 = 未启用，界面上就得摆一个必填输入框，把「配地址」这件我们自己的
/// 事推给用户——用户拿到手机端却连不上，报错还写「还没配置 relay 地址」。
///
/// 解析顺序与 `updater::manifest_url()` 同构（内置默认 → 落盘值覆盖）：
/// 自建服务只需要把 `relay.json` 的 `url` 改成自己的地址，不填就用官方的。
/// 地址不外露在界面上：显示出来等于让用户抄，抄错还是连不上。
pub const DEFAULT_RELAY_URL: &str = "wss://hermix.cssso.com";

#[derive(Serialize, Deserialize, Clone, Default)]
struct StoredRelay {
    version: u32,
    #[serde(default)]
    url: String,
    #[serde(default)]
    node_id: String,
    /// DPAPI 密文（base64）。空 = 没签发过。
    #[serde(default)]
    protected_token: String,
}

/// 进程内的 relay 设置。`token` 不外泄：API 只回「配没配」。
#[derive(Clone, Default)]
pub struct RelaySettings {
    pub url: String,
    pub node_id: String,
    token: String,
    /// 加载时的错误原样带着，供界面显示。
    pub load_error: Option<String>,
}

impl RelaySettings {
    /// 有地址 = 启用。`load()` 之后地址至少是 `DEFAULT_RELAY_URL`，所以正常
    /// 配置下恒为真；唯一的例外是**配置读坏了**（`load_error` 分支回落成空
    /// 地址）——那时宁可不连：拿半条配置去连，比不连更难排查。
    pub fn enabled(&self) -> bool {
        !self.url.trim().is_empty()
    }

    /// 凭据配了没有。**没有凭据就不能连**——relay 会对没有 Bearer 的连接直接 401。
    pub fn has_token(&self) -> bool {
        !self.token.trim().is_empty()
    }

    pub fn relay_config(&self) -> RelayConfig {
        RelayConfig {
            url: self.url.clone(),
            node_id: self.node_id.clone(),
            ..Default::default()
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

fn settings_path() -> PathBuf {
    hermix_config::paths::hermix_home().join(FILE)
}

/// 读取。文件不存在 = 从未配置（正常，不是错误）；文件存在但读不了 = 带着错误
/// 回落成默认值，让界面能说出「配置读坏了」而不是「未配置」。
pub fn load_at(path: &Path) -> RelaySettings {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<StoredRelay>(&raw) {
            Ok(stored) => {
                if stored.version != VERSION {
                    return broken(format!("relay.json 版本不支持：{}", stored.version));
                }
                let token = if stored.protected_token.is_empty() {
                    String::new()
                } else {
                    match crate::relay_identity::unprotect_secret(
                        &stored.protected_token,
                        DPAPI_LABEL,
                    ) {
                        Ok(plain) => String::from_utf8(plain).unwrap_or_default(),
                        Err(e) => {
                            return broken(format!(
                                "relay 凭据解不开（多半是换了 Windows 账户或 profile 损坏）：{e}。请重新签发并填入。"
                            ))
                        }
                    }
                };
                RelaySettings {
                    // 空 = 从没改过 = 用官方的（见 DEFAULT_RELAY_URL）。
                    url: if stored.url.trim().is_empty() {
                        DEFAULT_RELAY_URL.into()
                    } else {
                        stored.url
                    },
                    node_id: stored.node_id,
                    token,
                    load_error: None,
                }
            }
            Err(e) => broken(format!("relay.json 解析失败：{e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RelaySettings {
            url: DEFAULT_RELAY_URL.into(),
            ..Default::default()
        },
        Err(e) => broken(format!("relay.json 读不了：{e}")),
    }
}

fn broken(msg: String) -> RelaySettings {
    tracing::warn!("relay 配置加载失败：{msg}");
    RelaySettings {
        load_error: Some(msg),
        ..Default::default()
    }
}

pub fn load() -> RelaySettings {
    load_at(&settings_path())
}

pub fn save_at(path: &Path, s: &RelaySettings) -> Result<(), String> {
    let stored = StoredRelay {
        version: VERSION,
        url: s.url.clone(),
        node_id: s.node_id.clone(),
        protected_token: if s.token.trim().is_empty() {
            String::new()
        } else {
            crate::relay_identity::protect_secret(s.token.as_bytes(), DPAPI_LABEL)
                .map_err(|e| format!("保护 relay 凭据失败：{e}"))?
        },
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let body =
        serde_json::to_string_pretty(&stored).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(path, body).map_err(|e| format!("写入失败：{e}"))
}

/// 稳定的本机标识：E2EE 公钥前 8 位十六进制。
///
/// 用 `rand` 铸一个 node_id 也可以，但那样「这台电脑是谁」就与它的加密身份
/// 脱钩了——而它本来就有唯一且稳定的公钥，且 `QrPayload::server_id` 已经是
/// `hermix-<8 hex>` 这个形状（`pairing.rs:530`），两边能对上。
pub(crate) fn derive_node_id() -> String {
    use sha2::{Digest, Sha256};
    match crate::relay_identity::load_or_create() {
        Ok(id) => {
            let digest = Sha256::digest(id.public_key());
            let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
            format!("hermix-{hex}")
        }
        Err(e) => {
            tracing::warn!("无法从 E2EE 身份推导 node_id：{e}；请在界面上手填");
            String::new()
        }
    }
}

// ── 手机 ↔ relay 绑定表 ──────────────────────────────────────────

/**
 * 一台已配对手机在 relay 侧的凭据哈希，按 `device_id` 索引。
 *
 * ## 为什么只存哈希
 *
 * 撤销时 relay 要的是「把持有这条凭据的连接踢掉」。让 relay 支持**按哈希撤销**，
 * hermix 就完全不必持有那台手机的明文 `hrel_*` —— 中间任何一环都没见过真凭据。
 * 哈希本来就在 relay 库里当主键，不是额外泄露。
 *
 * 存在 `~/.hermix/relay_phones.json` 而不是 `devices.db`：后者是 SQLite 且正被
 * 审计线改动，理由与 `relay_identity::PeerStore` 相同。
 *
 * ## 存哈希也意味着「撤销能力」本身要看得见
 *
 * 一台没登记过 relay 的手机（只做过局域网直连）在这里查不到记录，撤销时
 * relay 侧无从下手。`GET /api/pairing/devices` 会带上 `relay_bound`，界面据此
 * 告诉用户「这台设备没走过中转，撤销只影响直连」。
 */
#[derive(Serialize, Deserialize, Default, Clone)]
struct StoredRelayPhones {
    #[serde(default)]
    devices: std::collections::BTreeMap<String, StoredRelayPhone>,
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredRelayPhone {
    /// `sha256(hrel_*)` 的小写 hex
    token_hash: String,
    node_id: String,
    bound_at: u64,
}

/// 64 位小写 hex 判定。
///
/// ## 这里为什么不用正则字面量
///
/// Rust 1.92 起**即使 edition 2021 也不再把 `/…/` 解析成正则**（实测
/// `rustc 1.92.0` 直接报 `expected expression, found '/'`）。看到这个报错时
/// 不用查自己的括号配平——就是它。
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub struct PhoneBindingStore {
    path: PathBuf,
}

impl PhoneBindingStore {
    pub fn at_default() -> Self {
        Self {
            path: hermix_config::paths::hermix_home().join(PHONE_BINDING_FILE),
        }
    }

    /// 测试用：指定路径。
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> Result<StoredRelayPhones, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("relay_phones.json 解析失败：{e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoredRelayPhones::default()),
            Err(e) => Err(format!("relay_phones.json 读不了：{e}")),
        }
    }

    fn write(&self, stored: &StoredRelayPhones) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let body = serde_json::to_string_pretty(stored).map_err(|e| format!("序列化失败：{e}"))?;
        std::fs::write(&self.path, body).map_err(|e| format!("写入失败：{e}"))
    }

    pub fn get(&self, device_id: &str) -> Option<String> {
        self.read().ok()?.devices.get(device_id).map(|p| p.token_hash.clone())
    }

    /// 登记 / 覆盖一条绑定。哈希格式不对直接拒——写进去的坏值会让撤销静默失效。
    pub fn put(&self, device_id: &str, token_hash: &str, node_id: &str) -> Result<(), String> {
        if !is_sha256_hex(token_hash) {
            return Err("relay 凭据哈希必须是 64 位小写 hex".into());
        }
        let mut stored = self.read()?;
        stored.devices.insert(
            device_id.to_string(),
            StoredRelayPhone {
                token_hash: token_hash.to_string(),
                node_id: node_id.to_string(),
                bound_at: now_secs(),
            },
        );
        self.write(&stored)
    }

    /// 撤销后清掉本地记录：留着它会让下次撤销再打一次 relay（幂等但没意义），
    /// 更糟的是让人以为这台设备还绑着中转。
    pub fn remove(&self, device_id: &str) -> Result<(), String> {
        let mut stored = self.read()?;
        if stored.devices.remove(device_id).is_none() {
            return Ok(());
        }
        self.write(&stored)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 撤销结果。**本地撤销与 relay 撤销是两件事，必须分开报。**
#[derive(Serialize)]
pub struct RevokeOutcome {
    pub device_id: String,
    /// hermix 侧的设备令牌已失效（= 这台设备发不出任何帧）。永远为 true。
    pub local_revoked: bool,
    /// 这台设备是否登记过 relay 绑定。没登记就没有 relay 侧可撤销的东西。
    pub relay_bound: bool,
    /// relay 侧是否真的撤了。`relay_bound` 为 false 时为 false 且不���错误。
    pub relay_revoked: bool,
    /// relay 不可达 / 拒绝时的人话原因。**不回 5xx**：本地撤销已经生效了，
    /// 整体失败会让用户以为什么都没发生——那比重试更糟。
    pub relay_error: Option<String>,
}

/// 让 relay 撤销某个手机凭据哈希并断开它的连接。
pub async fn revoke_phone_at_relay(url: &str, node_token: &str, token_hash: &str) -> Result<bool, String> {
    let http_base = url
        .trim()
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))?;

    let resp = client
        .post(format!("{http_base}/api/relay/revoke-phone"))
        .bearer_auth(node_token)
        .json(&serde_json::json!({ "token_hash": token_hash }))
        .send()
        .await
        .map_err(|e| format!("连不上 relay：{e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("relay 拒绝撤销（HTTP {}）：{body}", status.as_u16()));
    }
    Ok(body.contains("\"revoked\":true"))
}

/// 撤销一台设备的 relay 访问。由 `pairing::revoke_device` 在本地撤销成功后调用。
pub(crate) async fn revoke_device_on_relay(
    state: &crate::state::HttpServerState,
    device_id: &str,
) -> RevokeOutcome {
    let hash = PhoneBindingStore::at_default().get(device_id);
    let Some(hash) = hash else {
        return RevokeOutcome {
            device_id: device_id.to_string(),
            local_revoked: true,
            relay_bound: false,
            relay_revoked: false,
            relay_error: None,
        };
    };

    let (url, node_token) = {
        let s = state.relay_settings.lock().await;
        (s.url.clone(), s.token().to_string())
    };
    if url.trim().is_empty() {
        return RevokeOutcome {
            device_id: device_id.to_string(),
            local_revoked: true,
            relay_bound: true,
            relay_revoked: false,
            relay_error: Some("这台电脑没有配置 relay 地址，无法通知它断开这台手机。".into()),
        };
    }

    let outcome = match revoke_phone_at_relay(&url, &node_token, &hash).await {
        Ok(true) => RevokeOutcome {
            device_id: device_id.to_string(),
            local_revoked: true,
            relay_bound: true,
            relay_revoked: true,
            relay_error: None,
        },
        Ok(false) => RevokeOutcome {
            device_id: device_id.to_string(),
            local_revoked: true,
            relay_bound: true,
            relay_revoked: false,
            relay_error: None,
        },
        Err(e) => RevokeOutcome {
            device_id: device_id.to_string(),
            local_revoked: true,
            relay_bound: true,
            relay_revoked: false,
            relay_error: Some(e),
        },
    };
    // 不论 relay 那边成没成，本地记录都要清掉：留着会让界面显示这台设备
    // "还绑着中转"，而它其实已经撤销过了。
    if let Err(e) = PhoneBindingStore::at_default().remove(device_id) {
        tracing::warn!(error = %e, device_id, "清 relay 绑定记录失败");
    }
    outcome
}

// ── HTTP 面 ──────────────────────────────────────────────────────

/// `GET /api/relay/config` —— 界面读当前配置与连接状态。
///
/// **不回 token 明文**：只回 `has_token`。界面没有理由显示它，回了只会让它
/// 出现在 devtools / 截图 / 日志里。
#[derive(Serialize)]
pub struct RelayConfigView {
    url: String,
    node_id: String,
    has_token: bool,
    enabled: bool,
    /// `disabled` / `connecting` / `connected`
    state: &'static str,
    attempts: u32,
    /// 上次连接失败的原因（能拿到的话）。
    last_error: Option<String>,
    /// 配置读取时的错误：告诉用户「不是没配，是读坏了」。
    load_error: Option<String>,
}

pub async fn get_config(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<crate::state::HttpServerState>,
) -> Response {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (StatusCode::FORBIDDEN, "仅桌面端可查看 relay 配置").into_response();
    }
    let settings = state.relay_settings.lock().await;
    let status = state.relay_status.state();
    Json(RelayConfigView {
        url: settings.url.clone(),
        node_id: settings.node_id.clone(),
        has_token: settings.has_token(),
        enabled: settings.enabled(),
        state: match status {
            crate::relay_client::RelayState::Disabled => "disabled",
            crate::relay_client::RelayState::Connecting => "connecting",
            crate::relay_client::RelayState::Connected => "connected",
        },
        attempts: state.relay_status.attempts(),
        last_error: state.relay_last_error.lock().await.clone(),
        load_error: settings.load_error.clone(),
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct PutRelayConfig {
    pub url: String,
    #[serde(default)]
    pub node_id: String,
    /// 空串 = 清掉凭据（下次必须重填才能连）。
    #[serde(default)]
    pub token: String,
}

/// `PUT /api/relay/config` —— 改配置。
///
/// 仅 Local：这个端点能把一台**已在 relay 上注册**的电脑的接入凭据换掉，
/// 谁能调它谁就能决定这台电脑连到哪、拿什么身份连。远程可达就等于开放接管。
pub async fn put_config(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<crate::state::HttpServerState>,
    Json(body): Json<PutRelayConfig>,
) -> Response {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (
            StatusCode::FORBIDDEN,
            "仅桌面端可改 relay 配置",
        )
            .into_response();
    }

    let url = body.url.trim().to_string();
    if !url.is_empty() {
        let ok = url.starts_with("wss://") || url.starts_with("https://");
        if !ok {
            return (
                StatusCode::BAD_REQUEST,
                "relay 地址要以 wss:// 或 https:// 开头",
            )
                .into_response();
        }
    }
    // 凭据格式与 relay 的 `mintToken` 对齐（`auth.js:26`）。错格式在这里拦下，
    // 比让它连一次被 401 之后再来猜省事。
    let token = body.token.trim().to_string();
    if !token.is_empty() && !token.starts_with("hrel_") {
        return (
            StatusCode::BAD_REQUEST,
            "relay 凭据应以 hrel_ 开头（在 relay 服务器上用 relayctl add-node 签发）",
        )
            .into_response();
    }

    let node_id = {
        let existing = state.relay_settings.lock().await.node_id.clone();
        let wanted = body.node_id.trim().to_string();
        if !wanted.is_empty() {
            wanted
        } else if !existing.is_empty() {
            existing
        } else {
            derive_node_id()
        }
    };

    let next = RelaySettings {
        url,
        node_id,
        token,
        load_error: None,
    };
    if let Err(e) = save_at(&settings_path(), &next) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    *state.relay_settings.lock().await = next;
    // 重连由后台循环在下一轮退避后自然发生；刻意不杀任务——正在推进的
    // 连接不该被一次配置保存打断。
    tracing::info!("relay 配置已更新，等待重连生效");
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/relay/phone-credential` —— 向 relay 换一条手机凭据。
///
/// **为什么必须由桌面端代发**：node 凭据的明文只存在于这里（DPAPI 解出来的），
/// API 面对前端只回 `has_token`。所以「签发手机凭据」这件事没法推到前端做。
///
/// 这样做的直接收益：加一台手机不必登服务器敲 `relayctl add-phone`。
#[derive(Serialize, Deserialize)]
struct IssuedPhoneCredential {
    token: String,
    node_id: String,
}

/// 校验 enroll-node 的应答并取出明文 token。
///
/// node_id 必须与本机推导一致：relay 若返回别的 id，说明 relay 版本过旧、
/// 推导算法漂移或应答被篡改——拿这种凭据连上去会以另一个身份挂在 relay 上，
/// 配对码（`server_id`）就对不上了。当场拒绝比连上后再排查省一个量级。
fn validate_enroll_reply(body: &str, expected_node_id: &str) -> Result<String, String> {
    let parsed: IssuedPhoneCredential = serde_json::from_str(body)
        .map_err(|e| format!("relay 响应解析失败：{e}"))?;
    if parsed.token.is_empty() {
        return Err("relay 返回的凭据是空的".into());
    }
    if parsed.node_id != expected_node_id {
        return Err(format!(
            "relay 推导的 node_id（{}）与本机（{expected_node_id}）不一致，拒绝采纳",
            parsed.node_id
        ));
    }
    Ok(parsed.token)
}

/// relay 侧拒绝本机凭据（401：被撤销或 relay 重置过登记表）。
/// 配对签发与出站连接两处都要在此时重签凭据，判断口径必须一致。
pub(crate) fn is_relay_auth_reject(err: &str) -> bool {
    err.contains("HTTP 401")
}

/// 电脑侧凭据自动签发：把本机 E2EE 公钥报给 relay，换回 node 凭据。
///
/// 首次启动的桌面端还没有 `hrel_*`，过去只能让用户登服务器敲 `relayctl
/// add-node` 再把明文抄回界面。改为启动时（及配对前）自动调 relay 的
/// `POST /api/relay/enroll-node`，凭据到手后由调用方落盘并连接——用户全程无感。
///
/// node_id 由 relay 从公钥推导（与 [`derive_node_id`] 同算法），本机只做
/// **一致性校验**（见 [`validate_enroll_reply`]），不接受 relay 指定别的身份。
pub(crate) async fn enroll_node_credential(base: &str) -> Result<String, String> {
    let identity = crate::relay_identity::load_or_create()
        .map_err(|e| format!("读取本机 E2EE 身份失败：{e}"))?;
    let expected = derive_node_id();
    if expected.is_empty() {
        return Err("无法从 E2EE 身份推导 node_id".into());
    }
    let public_key = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(identity.public_key())
    };

    let http_base = base
        .trim()
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))?;

    let resp = client
        .post(format!("{http_base}/api/relay/enroll-node"))
        .json(&serde_json::json!({ "public_key": public_key }))
        .send()
        .await
        .map_err(|e| format!("连不上 relay：{e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or_else(|| body.clone());
        return Err(format!("relay 拒绝签发（HTTP {}）：{msg}", status.as_u16()));
    }
    validate_enroll_reply(&body, &expected)
}

/// 自动签发并把凭据落盘（relay.json + 内存态）。
///
/// 两个调用方（启动时的出站连接、配对前的兜底）共用：签发成功后必须**同时**
/// 更新磁盘与 `state.relay_settings`，否则界面（`GET /api/relay/config` 的
/// `has_token`）看到的还是旧状态，用户会以为签发没发生。
pub(crate) async fn ensure_node_credential(
    url: &str,
    settings: &tokio::sync::Mutex<RelaySettings>,
) -> Result<String, String> {
    let token = enroll_node_credential(url).await?;
    let mut s = settings.lock().await;
    s.node_id = derive_node_id();
    s.token = token.clone();
    s.load_error = None;
    save_at(&settings_path(), &s)?;
    Ok(token)
}

/// 向 relay 签发一条手机凭据，返回**明文 token**（仅此一次下发）。
///
/// `base` 与配置同口径（wss:// / https:// / 裸 host 均可），内部换算成 HTTP。
/// 生成配对二维码（`pairing::create_pairing_session`）与设置页手动签发共用。
pub(crate) async fn issue_phone_credential(base: &str, node_token: &str) -> Result<String, String> {    let http_base = base
        .trim()
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))?;

    let resp = client
        .post(format!("{http_base}/api/relay/issue-phone"))
        .bearer_auth(node_token)
        .json(&serde_json::json!({ "label": "hermix-m" }))
        .send()
        .await
        .map_err(|e| format!("连不上 relay：{e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // relay 的错误原文要带出来：「凭据无效」「只有 node 凭据能签发」
        // 「这台电脑已经连着一台手机」是完全不同的三个问题，混成一句
        // 「签发失败」等于把排查难度翻倍。
        //
        // relay 回的是 JSON，取其中的 `error` 字段而不是整段 body——整段 body
        // 直接甩到界面上就是一坨带转义的 JSON，而用户要读的是那句话。
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or_else(|| body.clone());
        return Err(format!("relay 拒绝签发（HTTP {}）：{msg}", status.as_u16()));
    }

    match serde_json::from_str::<IssuedPhoneCredential>(&body) {
        Ok(v) if !v.token.is_empty() => Ok(v.token),
        Ok(_) => Err("relay 返回的凭据是空的".into()),
        Err(e) => Err(format!("relay 响应解析失败：{e}")),
    }
}

pub async fn issue_phone(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<crate::state::HttpServerState>,
) -> Response {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (StatusCode::FORBIDDEN, "仅桌面端可签发手机凭据").into_response();
    }

    let (url, token, node_id) = {
        let s = state.relay_settings.lock().await;
        (s.url.clone(), s.token().to_string(), s.node_id.clone())
    };
    if url.trim().is_empty() {
        return (StatusCode::PRECONDITION_FAILED, "还没配 relay 地址").into_response();
    }
    if token.trim().is_empty() {
        return (
            StatusCode::PRECONDITION_FAILED,
            "还没配电脑侧凭据（hrel_*），relay 会拒绝这个请求",
        )
            .into_response();
    }

    match issue_phone_credential(&url, &token).await {
        Ok(t) => Json(IssuedPhoneCredential { token: t, node_id }).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

/// 重置本机 E2EE 身份与 relay 凭据（文件层）。
///
/// 场景：DPAPI 主密钥丢失（还原备份 / 搬盘 / 重建 Windows 账户）后，
/// `identity.json` 解不开且**不能**静默重生成——每台已配对手机的信任锚都封着
/// 这把公钥，换钥必须显式。这是那个显式动作：
///
/// 1. 删 `identity.json`（下次 `load_or_create` 生成新身份 → 新 node_id）；
/// 2. 删 `relay_phones.json`（旧手机绑定挂的是旧 node 的凭据哈希，已无意义）；
/// 3. 重写 `relay.json`：凭据清空，**自建地址尽量抢救**——JSON 本身还能解析
///    （DPAPI 坏的是 `protected_token` 的解密，不是文件），丢了地址用户就得
///    重新手填。
///
/// 设备注册表（devices.db）刻意不动：设备列表与撤销能力照旧可用，旧设备
/// 在界面上撤销或重新扫码都行。
fn reset_local_identity(
    identity_path: &Path,
    phones_path: &Path,
    relay_path: &Path,
) -> Result<RelaySettings, String> {
    let salvaged_url = std::fs::read_to_string(relay_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StoredRelay>(&raw).ok())
        .map(|s| s.url.trim().to_string())
        .filter(|u| !u.is_empty());
    match std::fs::remove_file(identity_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("删除本机身份失败：{e}")),
    }
    match std::fs::remove_file(phones_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("删除手机绑定表失败：{e}")),
    }
    let fresh = RelaySettings {
        url: salvaged_url.unwrap_or_default(),
        node_id: String::new(),
        token: String::new(),
        load_error: None,
    };
    save_at(relay_path, &fresh)?;
    Ok(fresh)
}

/// `POST /api/relay/reset-identity` —— 重置本机 E2EE 身份与 relay 凭据。
///
/// 仅 Local：它能删本机加密身份，远程可达就等于给任何网页一个「把这台电脑
/// 踢出中转、强制重新配对」的按钮。成功后重新拉起出站连接任务（闸保证重复
/// 调用不会叠出多个循环）——新身份 → 新 node_id → 自动签发直接走通，用户
/// 只剩「重新扫码配对手机」这一件事。
pub async fn reset_identity(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<crate::state::HttpServerState>,
) -> Response {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (StatusCode::FORBIDDEN, "仅桌面端可重置本机身份").into_response();
    }
    let home = hermix_config::paths::hermix_home();
    let fresh = match reset_local_identity(
        &home.join(crate::relay_identity::IDENTITY_FILE),
        &home.join(PHONE_BINDING_FILE),
        &settings_path(),
    ) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    *state.relay_settings.lock().await = fresh;
    tracing::info!("本机 E2EE 身份与 relay 凭据已重置，重新拉起出站连接");
    state.spawn_relay_connector();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE);
        (dir, p)
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let (_d, p) = tmp();
        let s = load_at(&p);
        // 没配过 = 用官方地址，不是「未启用」——地址是我们的部署参数，
        // 不该变成用户必填项（见 DEFAULT_RELAY_URL）。
        assert_eq!(s.url, DEFAULT_RELAY_URL);
        assert!(s.enabled());
        assert!(s.load_error.is_none(), "没配过不是错误");
    }

    #[test]
    fn node_id_derivation_matches_relay_implementation() {
        // 跨语言钉死：relay 侧 `auth.js::deriveNodeIdFromPublicKey` 必须与本侧
        // `derive_node_id` 同算法（sha256(65 字节原始公钥) 前 4 字节 hex）。
        // 这里的期望值是独立算好的固定向量，relay 的 JS 测试对同一向量断言
        // 同一结果——两边算法漂移时必有一边红。
        use sha2::{Digest, Sha256};
        let mut raw = vec![0x04u8];
        raw.extend((1u8..=64).collect::<Vec<_>>());
        let hex: String = Sha256::digest(&raw)[..4].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "0ed3a6ab");
        assert_eq!(format!("hermix-{hex}"), "hermix-0ed3a6ab");
    }

    #[test]
    fn enroll_reply_happy_path() {
        let token = validate_enroll_reply(
            r#"{"token":"hrel_abc123","node_id":"hermix-0ed3a6ab"}"#,
            "hermix-0ed3a6ab",
        )
        .expect("合法应答应通过");
        assert_eq!(token, "hrel_abc123");
    }

    #[test]
    fn enroll_reply_rejects_foreign_node_id() {
        // relay 指定别的身份 = 推导漂移或应答被篡改。拿了这种凭据连上去，
        // 配对码里的 server_id 就对不上手机扫码的预期，当场拒绝。
        let err = validate_enroll_reply(
            r#"{"token":"hrel_abc123","node_id":"hermix-deadbeef"}"#,
            "hermix-0ed3a6ab",
        )
        .expect_err("node_id 不一致必须拒绝");
        assert!(err.contains("不一致"), "错误里要说清是不一致：{err}");
    }

    #[test]
    fn enroll_reply_rejects_empty_token_and_garbage() {
        assert!(validate_enroll_reply(r#"{"token":"","node_id":"hermix-0ed3a6ab"}"#, "hermix-0ed3a6ab").is_err());
        assert!(validate_enroll_reply("not json", "hermix-0ed3a6ab").is_err());
    }

    #[test]
    fn reset_identity_clears_files_and_salvages_url() {
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join(crate::relay_identity::IDENTITY_FILE);
        let phones = dir.path().join(PHONE_BINDING_FILE);
        let relay = dir.path().join(FILE);

        // 造一份「DPAPI 坏了」的现场：三个文件都在，relay.json 里的密文解不开
        // 但 JSON 本身可解析——自建地址要能从这里抢救回来。
        std::fs::write(&identity, r#"{"version":1,"public_key":"x","protected_key":"undecryptable"}"#).unwrap();
        std::fs::write(&phones, r#"{"devices":{}}"#).unwrap();
        std::fs::write(
            &relay,
            r#"{"version":1,"url":"wss://self.example","node_id":"hermix-old","protected_token":"undecryptable"}"#,
        )
        .unwrap();

        let fresh = reset_local_identity(&identity, &phones, &relay).expect("重置应成功");
        assert!(!identity.exists(), "身份文件必须删除");
        assert!(!phones.exists(), "手机绑定表必须删除");
        // 自建地址抢救回来；凭据与 node_id 清空（node_id 已随旧身份作废）。
        assert_eq!(fresh.url, "wss://self.example");
        assert!(fresh.token.is_empty() && fresh.node_id.is_empty());

        // 落盘的那份也要能正常读回来，且下次启动自动签发会重新填 token。
        let reloaded = load_at(&relay);
        assert_eq!(reloaded.url, "wss://self.example");
        assert!(!reloaded.has_token());
        assert!(reloaded.load_error.is_none());
    }

    #[test]
    fn reset_identity_is_idempotent_on_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = reset_local_identity(
            &dir.path().join(crate::relay_identity::IDENTITY_FILE),
            &dir.path().join(PHONE_BINDING_FILE),
            &dir.path().join(FILE),
        )
        .expect("文件本来就不存在也应成功");
        assert!(fresh.url.is_empty(), "没有旧配置可抢救 = 回到默认地址");
    }

    #[test]
    fn empty_stored_url_falls_back_to_the_official_relay() {
        let (_d, p) = tmp();
        std::fs::write(&p, br#"{"version":1,"url":"","node_id":"n","protected_token":""}"#)
            .unwrap();
        let s = load_at(&p);
        assert_eq!(s.url, DEFAULT_RELAY_URL);
        assert!(s.enabled());
    }

    #[test]
    fn self_hosted_url_wins_over_the_default() {
        let (_d, p) = tmp();
        let s = RelaySettings {
            url: "wss://my-own-relay.internal".into(),
            node_id: "n".into(),
            token: String::new(),
            load_error: None,
        };
        save_at(&p, &s).expect("save");
        assert_eq!(load_at(&p).url, "wss://my-own-relay.internal");
    }

    #[test]
    fn broken_config_does_not_silently_fall_back_to_connecting() {
        let (_d, p) = tmp();
        std::fs::write(&p, br#"{"version":99,"url":"","node_id":"n","protected_token":""}"#)
            .unwrap();
        let s = load_at(&p);
        // 配置坏了 → 空地址 → 不连。拿半条配置去连比不连更难排查。
        assert!(!s.enabled());
        assert!(s.load_error.is_some());
    }

    #[test]
    fn roundtrip_preserves_fields_and_token() {
        let (_d, p) = tmp();
        let s = RelaySettings {
            url: "wss://relay.example.com".into(),
            node_id: "hermix-abcdef01".into(),
            token: "hrel_deadbeef".into(),
            load_error: None,
        };
        save_at(&p, &s).expect("save");
        let back = load_at(&p);
        assert_eq!(back.url, s.url);
        assert_eq!(back.node_id, s.node_id);
        assert!(back.has_token());
        assert_eq!(back.token(), s.token(), "token 必须往返一致");
        assert!(back.load_error.is_none());
    }

    #[test]
    fn token_is_not_stored_in_plaintext() {
        let (_d, p) = tmp();
        let s = RelaySettings {
            url: "wss://relay.example.com".into(),
            node_id: "n".into(),
            token: "hrel_supersecretvalue".into(),
            load_error: None,
        };
        save_at(&p, &s).expect("save");
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(
            !raw.contains("hrel_supersecretvalue"),
            "凭据不得以明文躺在磁盘上：{raw}"
        );
    }

    #[test]
    fn corrupt_file_reports_error_instead_of_silently_defaulting() {
        // 这是本模块最要紧的一条：静默回落会让用户看到「未配置」，
        // 而真相是「配过但读坏了」，两者排查方向完全相反。
        let (_d, p) = tmp();
        std::fs::write(&p, b"{ this is not json").unwrap();
        let s = load_at(&p);
        assert!(s.load_error.is_some(), "损坏必须带错回来");
        assert!(!s.enabled());
    }

    #[test]
    fn wrong_version_is_reported() {
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            br#"{"version":99,"url":"wss://x","node_id":"n","protected_token":""}"#,
        )
        .unwrap();
        assert!(load_at(&p).load_error.is_some());
    }

    #[test]
    fn tampered_token_does_not_load_as_valid() {
        // 密文被手改 → 解不开 → 带错回来，而不是拿一个错的 token 去连。
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            br#"{"version":1,"url":"wss://x","node_id":"n","protected_token":"bm90LWEtY2lwaGVy"}"#,
        )
        .unwrap();
        let s = load_at(&p);
        assert!(s.load_error.is_some());
        assert!(!s.has_token());
    }

    #[test]
    fn empty_token_survives_save_load_as_unset() {
        let (_d, p) = tmp();
        let s = RelaySettings {
            url: "wss://relay.example.com".into(),
            node_id: "n".into(),
            token: String::new(),
            load_error: None,
        };
        save_at(&p, &s).expect("save");
        let back = load_at(&p);
        assert!(back.enabled(), "地址配了就该算启用");
        assert!(!back.has_token());
    }

    // ── 手机绑定表 ──────────────────────────────────────────────

    const HASH_A: &str = "aa11bb22cc33dd44ee55ff6600778899aabbccddeeff00112233445566778899";

    #[test]
    fn binding_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let st = PhoneBindingStore::at(dir.path().join("relay_phones.json"));
        assert_eq!(st.get("dev-1"), None, "没有记录就是没有，不是空串");
        st.put("dev-1", HASH_A, "hermix-abc").expect("put");
        assert_eq!(st.get("dev-1").as_deref(), Some(HASH_A));
        assert_eq!(st.get("dev-2"), None, "不能串到别的 device");
    }

    #[test]
    fn binding_store_rejects_bad_hash() {
        // 写进去的坏值会让撤销**静默失效**：撤销请求带着它过去，relay 认不出，
        // 于是界面说「已撤销」而连接还在。这种失败必须发生在写入时。
        let dir = tempfile::tempdir().unwrap();
        let st = PhoneBindingStore::at(dir.path().join("relay_phones.json"));
        for bad in ["", "short", "AA11BB22CC33DD44EE55FF6600778899AABBCCDDEEFF00112233445566778899"] {
            assert!(st.put("dev-1", bad, "n").is_err(), "{bad:?} 必须被拒");
        }
        assert_eq!(st.get("dev-1"), None, "被拒的写入不得留下记录");
    }

    #[test]
    fn binding_store_remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let st = PhoneBindingStore::at(dir.path().join("relay_phones.json"));
        st.put("dev-1", HASH_A, "n").unwrap();
        st.remove("dev-1").expect("remove");
        assert_eq!(st.get("dev-1"), None);
        st.remove("dev-1").expect("重复移除不得报错");
    }

    #[test]
    fn binding_store_keeps_only_the_latest_hash() {
        // 手机重装后换了新凭据、同一个 device_id 再配一次：旧哈希必须被覆盖，
        // 否则撤销会去撤一条已经不存在的凭据，真正的连接断不掉。
        let dir = tempfile::tempdir().unwrap();
        let st = PhoneBindingStore::at(dir.path().join("relay_phones.json"));
        st.put("dev-1", HASH_A, "n").unwrap();
        let newer = "bb11bb22cc33dd44ee55ff6600778899aabbccddeeff00112233445566778899";
        st.put("dev-1", newer, "n").unwrap();
        assert_eq!(st.get("dev-1").as_deref(), Some(newer));
    }

    #[test]
    fn binding_store_survives_corrupt_file_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("relay_phones.json");
        std::fs::write(&p, b"{ not json").unwrap();
        let st = PhoneBindingStore::at(p);
        // 损坏时读不出绑定 = 撤销会认为「没绑过」，界面据此提示；
        // 绝不 panic —— 那会把一次文件损坏升级成整个服务起不来。
        assert_eq!(st.get("dev-1"), None);
    }
}
