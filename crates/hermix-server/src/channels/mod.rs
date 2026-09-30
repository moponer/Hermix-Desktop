//! IM 远程指挥：让用户在自己手机上的 IM 里驱动本机会话。
//!
//! 只做国内 IM 的**官方机器人**通道。浏览器扫码那套（H5 页面）不在范围内：
//! 它本质是把桌面 API 暴露给浏览器，凭据在 URL 里流转，证书还得靠 TOFU，
//! 手机上体验也不如原生 IM。
//!
//! 形态抄 cc-haha 的 IM 适配器（`adapters/`），但不抄它的进程架构——那边
//! adapter 是独立进程，要自带 WS 桥 + 令牌 + 会话映射文件；这里直接长在
//! `hermix-server` 里。
//!
//! 授权抄的是 `pairing.rs` 那套（SQLite 设备表 + 审计 + 可撤销），不是
//! cc-haha 的 JSON append-only 白名单。IM 发来的 6 位码和网页扫码用的是
//! **同一个** `PairingSessions`：同样的 TTL、同样一次性、同样 5 次错误预算。
//!
//! 环境变量（凭据成对出现，缺一个就只读不启）：
//! - `HERMIX_IM_QQ_APP_ID` / `HERMIX_IM_QQ_APP_SECRET`
//! - `HERMIX_IM_WECOM_BOT_ID` / `HERMIX_IM_WECOM_SECRET`
//! - `HERMIX_IM_QQ_ALLOWED` / `HERMIX_IM_WECOM_ALLOWED`：逗号分隔的
//!   免配对直通 id。留空 = 只有配对过的人能进。

pub mod port;
pub mod provision;
pub mod qq;
pub mod session;
pub mod wecom;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use hermix_config::env_compat;

use crate::state::HttpServerState;

use self::port::{ImPort, Inbound};
use self::provision::ImCredentials;
use self::qq::QqApi;
use self::session::ChannelRuntime;
use self::wecom::WecomPort;


/// 扫码绑定拿到的凭据落在 `<hermix_home>/im_channels.json`。
///
/// ⚠️ 该文件含明文 bot secret，绝不可提交进仓库（.gitignore 已有条目）。
/// 前端通过 `/api/channels/bind/status` 只读脱敏后的 app_id，永远拿不到 secret。
///
/// 这些是能直接操控本机的机器人凭据，和 cc-haha 存 `adapters.json` 的取舍
/// 一样是明文——没有可用的系统钥匙串依赖，为它引一个不划算。文件权限在本机
/// 用户目录下，不要提交进仓库。
const CRED_FILE: &str = "im_channels.json";

fn cred_path() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join(CRED_FILE)
}

fn load_stored() -> HashMap<String, ImCredentials> {
    let Ok(text) = std::fs::read_to_string(cred_path()) else {
        return HashMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_stored(map: &HashMap<String, ImCredentials>) -> Result<(), String> {
    let path = cred_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    // 临时文件 + rename，避免写到一半断电留下半个 JSON。
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换失败：{e}"))
}

/// 一个已启用平台的凭据。凭据完整才启用，半截配置当没配。
struct PlatformCreds {
    platform: &'static str,
    primary: String,
    secondary: String,
    allowed: Vec<String>,
}

fn split_list(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_creds() -> Vec<PlatformCreds> {
    // 环境变量优先（给无头部署用），否则用扫码绑定落盘的凭据。
    let stored = load_stored();
    let mut out = Vec::new();
    let specs: [(&'static str, (&str, &str), (&str, &str), (&str, &str)); 2] = [
        (
            "qq",
            ("HERMIX_IM_QQ_APP_ID", "HERMES_IM_QQ_APP_ID"),
            ("HERMIX_IM_QQ_APP_SECRET", "HERMES_IM_QQ_APP_SECRET"),
            ("HERMIX_IM_QQ_ALLOWED", "HERMES_IM_QQ_ALLOWED"),
        ),
        (
            "wecom",
            ("HERMIX_IM_WECOM_BOT_ID", "HERMES_IM_WECOM_BOT_ID"),
            ("HERMIX_IM_WECOM_SECRET", "HERMES_IM_WECOM_SECRET"),
            ("HERMIX_IM_WECOM_ALLOWED", "HERMES_IM_WECOM_ALLOWED"),
        ),
    ];
    for (platform, a, b, allowed) in specs {
        let env_pair = (
            env_compat(a.0, a.1).filter(|v| !v.trim().is_empty()),
            env_compat(b.0, b.1).filter(|v| !v.trim().is_empty()),
        );
        let (primary, secondary) = match env_pair {
            (Some(p), Some(s)) => (p, s),
            _ => match stored.get(platform) {
                Some(c) => (c.primary.clone(), c.secondary.clone()),
                None => continue,
            },
        };
        out.push(PlatformCreds {
            platform,
            primary,
            secondary,
            allowed: split_list(env_compat(allowed.0, allowed.1)),
        });
    }
    out
}

/// 通道运行时句柄。`stop` 发信号后等各网关循环自己收尾。
pub struct ImRuntime {
    shutdown: watch::Sender<bool>,
    /// 放进 Mutex 是为了 `stop(&self)` 能把 JoinHandle 取出来 await ——
    /// 句柄本身在 `Arc<ImRuntime>` 后面，拿不到 `&mut`。
    handles: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// 平台 → 网关是否已连上（凭据在 ≠ 通道活着）。"机器人不回消息"时
    /// 用户唯一需要看的就是这两个字的差别，所以单独暴露成状态接口。
    connected: HashMap<&'static str, Arc<AtomicBool>>,
}

impl ImRuntime {
    /// 启动所有凭据齐全的平台。没有任何平台配全则返回 `None`。
    pub fn start(state: HttpServerState) -> Option<Arc<Self>> {
        let creds = read_creds();
        if creds.is_empty() {
            return None;
        }
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        let mut ports: Vec<Arc<dyn ImPort>> = Vec::new();
        let mut allowed: HashMap<&'static str, Vec<String>> = HashMap::new();
        // 收据留到 ChannelRuntime 建好之后再挂分发器——分发器需要 runtime。
        let mut inbound: Vec<(&'static str, mpsc::Receiver<Inbound>)> = Vec::new();
        let mut connected: HashMap<&'static str, Arc<AtomicBool>> = HashMap::new();

        for c in creds {
            let (tx, rx) = mpsc::channel(64);
            match c.platform {
                "qq" => {
                    let api = Arc::new(QqApi::new(c.primary.clone(), c.secondary.clone()));
                    ports.push(Arc::new(qq::QqPort::new(api.clone())));
                    let flag = Arc::new(AtomicBool::new(false));
                    connected.insert("qq", flag.clone());
                    handles.push(tokio::spawn(qq::run_gateway(
                        api, tx, shutdown_rx.clone(), flag,
                    )));
                }
                "wecom" => {
                    // 企微是纯 WS 出站：发送帧也得走那条 socket，所以端口和
                    // 网关共用一条双向命令通道。
                    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                    let mut port = WecomPort::new(c.primary.clone(), c.secondary.clone());
                    port.attach_sender(cmd_tx);
                    let gw = Arc::new(port);
                    ports.push(gw.clone());
                    handles.push(tokio::spawn(wecom::run_gateway(
                        gw,
                        tx,
                        shutdown_rx.clone(),
                        cmd_rx,
                    )));
                }
                other => {
                    tracing::warn!(platform = other, "未知的 IM 平台，已跳过");
                    continue;
                }
            }
            allowed.insert(c.platform, c.allowed);
            inbound.push((c.platform, rx));
        }

        let platforms: Vec<&'static str> = ports.iter().map(|p| p.platform()).collect();
        let runtime = ChannelRuntime::new(state, ports, allowed);

        // 网关 → 入站分发。每条消息一个 task，所以一条卡住的消息不会拖住
        // 整个通道；同一段私聊的串行性由 ChannelRuntime 里的聊天锁保证。
        for (platform, mut rx) in inbound {
            let rt = runtime.clone();
            handles.push(tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    let rt = rt.clone();
                    tokio::spawn(async move { rt.handle_inbound(platform, msg).await });
                }
            }));
        }

        tracing::info!(platforms = ?platforms, "IM 通道已启动");
        Some(Arc::new(Self {
            shutdown,
            handles: tokio::sync::Mutex::new(handles),
            connected,
        }))
    }

    /// 各平台网关连接状态（true = 已连上）。
    pub fn connection_status(&self) -> HashMap<&'static str, bool> {
        self.connected
            .iter()
            .map(|(k, v)| (*k, v.load(std::sync::atomic::Ordering::Relaxed)))
            .collect()
    }

    pub async fn stop(&self) {
        let _ = self.shutdown.send(true);
        let mut handles = std::mem::take(&mut *self.handles.lock().await);
        if handles.is_empty() {
            return;
        }
        // 网关可能正卡在一次网络读上，最多等 3 秒就走。
        let joined = tokio::time::timeout(Duration::from_secs(3), async {
            for h in handles.drain(..) {
                let _ = h.await;
            }
        })
        .await;
        if joined.is_err() {
            tracing::warn!("IM 通道未在 3s 内退出，已放弃等待");
        }
    }
}

// ---------------------------------------------------------------------------
// 运行时热重启
// ---------------------------------------------------------------------------
//
// 绑定/解绑改变的是**磁盘上的凭据**，而网关连接在启动时就按当时的凭据建好
// 了。原先只能提示"重启桌面端后生效"——用户扫码授权完还要杀一次应用重开，
// 这条链路在"能不能用"的判断上直接卡死。凭据一落盘就地重启通道：旧的停掉
// （最多等 3s），新的按最新凭据起。
//
// 槽位是全局单例：lib.rs 启动时 install 一次，之后所有改凭据的 handler
// 调 restart()。锁只用来换指针，绝不跨 await 持有（stop() 要 await）。

fn runtime_slot() -> &'static Mutex<Option<Arc<ImRuntime>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<ImRuntime>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 启动期登记当前运行时（关机路径仍读局部变量，这里只是给后续重启留个起点）。
pub fn install_runtime(rt: Option<Arc<ImRuntime>>) {
    if let Ok(mut g) = runtime_slot().lock() {
        *g = rt;
    }
}

/// 凭据变化后热重启 IM 通道。旧运行时停干净再起新的。
pub async fn restart_runtime(state: HttpServerState) {
    let old = match runtime_slot().lock() {
        Ok(mut g) => g.take(),
        Err(_) => None,
    };
    if let Some(rt) = old {
        rt.stop().await;
    }
    let fresh = ImRuntime::start(state);
    match runtime_slot().lock() {
        Ok(mut g) => *g = fresh.clone(),
        Err(_) => {}
    }
    match fresh {
        Some(_) => tracing::info!("IM 通道已按新凭据热重启"),
        None => tracing::info!("IM 通道已停（无平台凭据齐全）"),
    }
}

// ---------------------------------------------------------------------------
// HTTP：铸一枚 IM 配对码（仅本机）
// ---------------------------------------------------------------------------

/// `POST /api/channels/pairing-code` — 铸一枚只给 IM 用的 6 位码。
///
/// 和 `/api/pairing/sessions` 分开是因为负载形态不同：这枚码没有二维码、
/// 也不依赖 TLS 监听器，它就是一行要抄到 IM 私聊里的话。共用同一个
/// `PairingSessions`，所以 TTL / 一次性 / 错误预算三件事天然一致。
///
/// 平台放在请求体里而不是路径上：配对码本身与平台无关，发到哪个平台就绑
/// 哪个平台。
pub(crate) async fn create_pairing_code(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<HttpServerState>,
    Json(body): Json<PairingCodeRequest>,
) -> Response {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (StatusCode::FORBIDDEN, "只能在桌面端本地生成 IM 配对码").into_response();
    }
    let platform = body.platform.trim().to_ascii_lowercase();
    if platform.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 platform").into_response();
    }
    let code = state.pairing_sessions.create_code_only();
    Json(serde_json::json!({
        "platform": platform,
        "pairing_code": code,
        "ttl_secs": crate::pairing::PAIRING_TTL.as_secs(),
        "single_use": true,
        "instructions": "在 IM 私聊机器人窗口里把这枚 6 位码发过去。",
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct PairingCodeRequest {
    pub platform: String,
}

// ---------------------------------------------------------------------------
// HTTP：扫码绑定（仅本机）
// ---------------------------------------------------------------------------

/// 扫码会话表。它是进程内的：桌面端重启后未完成的扫码作废，重新生成即可。
fn bind_sessions() -> &'static provision::BindSessions {
    static SESSIONS: std::sync::OnceLock<provision::BindSessions> = std::sync::OnceLock::new();
    SESSIONS.get_or_init(provision::BindSessions::new)
}

fn require_local(origin: &hermix_core::Origin) -> Option<Response> {
    if matches!(origin, hermix_core::Origin::Local) {
        return None;
    }
    Some((StatusCode::FORBIDDEN, "IM 绑定只能在桌面端本地操作").into_response())
}

/// 已绑定平台一览（凭据只回掩码，不回明文）。
#[derive(Serialize)]
struct BindStatusItem {
    platform: &'static str,
    bound: bool,
    id_preview: String,
    source: &'static str,
}

#[derive(Serialize)]
struct BindStatusResponse {
    platforms: Vec<BindStatusItem>,
}

/// `GET /api/channels/bind/status` — 各平台是否已绑定。
pub(crate) async fn bind_status(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    let stored = load_stored();
    let mut platforms = Vec::new();
    for platform in ["qq", "wecom"] {
        // 两个凭据都算才算数：只设一半的话通道根本起不来，显示"已绑定"
        // 是在骗用户。
        let env_set = match platform {
            "qq" => {
                env_compat("HERMIX_IM_QQ_APP_ID", "HERMES_IM_QQ_APP_ID").is_some()
                    && env_compat("HERMIX_IM_QQ_APP_SECRET", "HERMES_IM_QQ_APP_SECRET").is_some()
            }
            _ => {
                env_compat("HERMIX_IM_WECOM_BOT_ID", "HERMES_IM_WECOM_BOT_ID").is_some()
                    && env_compat("HERMIX_IM_WECOM_SECRET", "HERMES_IM_WECOM_SECRET").is_some()
            }
        };
        let cred = stored.get(platform);
        let bound = env_set || cred.is_some();
        platforms.push(BindStatusItem {
            platform,
            bound,
            id_preview: cred.map(|c| mask_id(&c.primary)).unwrap_or_default(),
            source: if env_set { "env" } else if cred.is_some() { "file" } else { "" },
        });
    }
    Json(BindStatusResponse { platforms }).into_response()
}

/// 凭据 id 只给前后若干位。够用户确认「绑的是哪个机器人」，不够被抄走复用。
fn mask_id(id: &str) -> String {
    if id.len() <= 8 {
        return id.to_string();
    }
    format!("{}…{}", &id[..4], &id[id.len() - 4..])
}

#[derive(Deserialize)]
pub struct StartBindRequest {
    platform: String,
}

/// `POST /api/channels/bind/start` — 起一次扫码绑定。
pub(crate) async fn bind_start(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    Json(body): Json<StartBindRequest>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    let platform = body.platform.trim().to_ascii_lowercase();
    let source = "hermix";
    match provision::start(bind_sessions(), &platform, source).await {
        Ok((session_key, verification_url)) => Json(serde_json::json!({
            "session_key": session_key,
            "verification_url": verification_url,
            "poll_interval_secs": 3,
            "ttl_secs": 600,
        }))
        .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct PollBindQuery {
    session_key: String,
}

/// `GET /api/channels/bind/poll?session_key=…` — 轮询绑定结果。
pub(crate) async fn bind_poll(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<HttpServerState>,
    Query(q): Query<PollBindQuery>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    let Some(platform) = bind_sessions().platform_of(&q.session_key) else {
        return Json(provision::BindPoll::NotStarted {
            message: "绑定会话不存在或已过期，请重新生成二维码。".to_string(),
        })
        .into_response();
    };
    let result = provision::poll(bind_sessions(), platform, &q.session_key).await;
    // 成功的那一次顺手把凭据落盘，下次启动就能直接起通道。
    if let provision::BindPoll::Connected { credentials } = &result {
        let mut stored = load_stored();
        stored.insert(platform.to_string(), credentials.clone());
        if let Err(e) = save_stored(&stored) {
            tracing::warn!(error = %e, "IM 凭据落盘失败，重启后需要重新扫码");
        } else {
            // 凭据已就位 → 通道就地重启，用户不用杀应用重开。
            restart_runtime(state).await;
        }
    }
    Json(result).into_response()
}

#[derive(Serialize)]
pub struct ChannelStatusItem {
    pub platform: String,
    pub bound: bool,
    /// 网关是否已连上。`bound && !connected` = 凭据在但通道没活（网络 /
    /// 沙箱 / 凭据过期），这是"机器人不回消息"最常见的一档。
    pub connected: bool,
}

/// `GET /api/channels/status` — 凭据 + 网关连接状态合并视图（仅本机）。
pub(crate) async fn channel_status(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    let stored = load_stored();
    let live = match runtime_slot().lock() {
        Ok(g) => g.as_ref().map(|rt| rt.connection_status()).unwrap_or_default(),
        Err(_) => Default::default(),
    };
    let items: Vec<ChannelStatusItem> = ["qq", "wecom"]
        .into_iter()
        .map(|p| ChannelStatusItem {
            platform: p.to_string(),
            bound: stored.contains_key(p),
            connected: live.get(p).copied().unwrap_or(false),
        })
        .collect();
    Json(serde_json::json!({ "platforms": items })).into_response()
}

#[derive(Deserialize)]
pub struct CancelBindRequest {
    session_key: String,
}

/// `POST /api/channels/bind/cancel` — 放弃一次进行中的绑定。
pub(crate) async fn bind_cancel(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    Json(body): Json<CancelBindRequest>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    provision::cancel(bind_sessions(), &body.session_key);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct UnbindRequest {
    platform: String,
}

/// `POST /api/channels/bind/unbind` — 删掉落盘凭据。
///
/// 已经配对过的 IM 账号**不**在这里被删——那是设备表里的条目，去「已配对
/// 设备」那一节撤销。解绑只是让通道下次启动不再连。
pub(crate) async fn bind_unbind(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<HttpServerState>,
    Json(body): Json<UnbindRequest>,
) -> Response {
    if let Some(r) = require_local(&origin) {
        return r;
    }
    let mut stored = load_stored();
    let removed = stored.remove(body.platform.trim()).is_some();
    if let Err(e) = save_stored(&stored) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    // 凭据没了 → 该平台的网关连接得停，否则它拿着已删凭据继续重连刷日志。
    if removed {
        restart_runtime(state).await;
    }
    Json(serde_json::json!({ "removed": removed })).into_response()
}
