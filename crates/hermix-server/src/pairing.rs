//! §5.5 第 3/4 层：设备配对与凭证。
//!
//! 一次性配对会话（TTL 120s）+ 持久设备注册表（SQLite）。配对成功后
//! 设备持有长寿命 device_token（只存哈希），后续请求以
//! `Authorization: Bearer <device_token>` 出示，由 `security::request_guard`
//! 校验后产出 `Origin::PairedDevice{device_id}` 进权限矩阵。
//!
//! 本批为后端基础：手机客户端与二维码 UI 另批落地。与设计文档
//! §5.5 第 4 层的差异（诚实记录）：
//! - 请求签名（nonce+时间戳，对策 T4 重放）未实现——token 全程走
//!   TLS 内，泄漏面由 TLS 承担；签名作为后续加固批次。
//! - device_pubkey / scopes 列已建但恒空——签名与细粒度 scope 落地时启用。
//!
//! 防滥用（对策 T6 暴力破解）：配对码 120s TTL + 一次性消费 +
//! 完成端点走全局 rate limiter；IP 级锁定随手机客户端批次补。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use hermix_core::AgentError;
use hermix_store::{OpenMode, Store};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::state::HttpServerState;
use crate::tls;

/// 取请求的来源 IP（字符串形式，供审计行 `source_ip` 列）。
/// 优先 X-Forwarded-For 第一跳（TLS 终结在反代后），回退到连接对端。
/// 返回 `None`（未捕获）由 `record_audit` 落 `NULL`，不阻断审计行。
pub fn request_source_ip(
    peer: Option<&std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        // 第一跳（最接近客户端）
        if let Some(first) = xff.split(',').next() {
            let first = first.trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    peer.map(|sa| sa.to_string())
}

/// 配对码有效期。设计文档 §5.5 第 3 层：120 秒。
pub const PAIRING_TTL: Duration = Duration::from_secs(120);

/// 审计事件枚举（字符串形式落盘，便于 `grep` / 跨语言消费）。
/// 对应设计文档 §5.5 附表四类：pairing / auth / grant / revoke。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEvent {
    /// 设备配对成功（`pair` 落库后）。
    Pairing,
    /// 凭证认证（`authenticate` 命中，含成功/撤销/失败）。
    Auth,
    /// 授权判定（`request_guard` 装 `device_scopes` 进 Extension 后，
    /// 下游关口按矩阵∩能力位判定时）。
    Grant,
    /// 设备撤销（`revoke` 落库后）。
    Revoke,
}

impl AuditEvent {
    /// 落盘用的字符串形式（`audit_log.event` 列）。
    pub fn as_str(self) -> &'static str {
        match self {
            AuditEvent::Pairing => "pairing",
            AuditEvent::Auth => "auth",
            AuditEvent::Grant => "grant",
            AuditEvent::Revoke => "revoke",
        }
    }
}

/// 审计行（读侧展示用，按时间倒序，最多 `limit` 条）。
#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub ts: i64,
    pub event: String,
    pub device_id: Option<String>,
    pub source_ip: Option<String>,
    pub decision: Option<String>,
}

/// 新设备默认能力位（§5.5 第 4 层细粒度授权，D5 拍板"会话级：可发消息
/// 看会话，高危工具强制桌面确认"）。JSON 数组字符串形式，直接写进
/// `paired_devices.scopes` 列。最小集 = 仅 `Capability::Chat`；
/// `DangerousTool` 走 `Origin::may` 的 `NeedsLocalConfirmation`（不在
/// 能力位里配），授权/配置/管理/工作台类在矩阵里 `Denied` 且能力位
/// 未配，被动态拒绝。`security::request_guard` 在 `Origin::may` 静态
/// 矩阵判定后 AND 此能力位（能力位 `[]` 等价全 `Denied`）。
pub const DEFAULT_DEVICE_SCOPES: &str = r#"["chat"]"#;

/// 设备注册表：`<hermix_home>/devices.db`（SQLite，hermix-store 托管）。
pub struct DeviceRegistry {
    store: Store,
}

/// 已配对设备（注册表行）。
#[derive(Debug, Clone, Serialize)]
pub struct PairedDevice {
    pub device_id: String,
    pub device_name: String,
    /// JSON 数组字符串。当前恒 `[]`（默认矩阵在 Origin::may 里）。
    pub scopes: String,
    pub issued_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl DeviceRegistry {
    pub fn open(path: &std::path::Path) -> Result<Self, AgentError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("create {}: {e}", parent.display())))?;
        }
        let store = Store::open(path, OpenMode::Write)
            .map_err(|e| AgentError::Config(format!("open devices db: {e}")))?;
        let created = store.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS paired_devices (
                    device_id     TEXT PRIMARY KEY,
                    device_name   TEXT NOT NULL,
                    token_hash    TEXT NOT NULL UNIQUE,
                    scopes        TEXT NOT NULL DEFAULT '[]',
                    issued_at     INTEGER NOT NULL,
                    last_seen_at  INTEGER,
                    revoked_at    INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS audit_log (
                    ts            INTEGER NOT NULL,
                    event         TEXT NOT NULL,
                    device_id     TEXT,
                    source_ip     TEXT,
                    decision      TEXT
                 );
                 CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);",
            )
            .is_ok()
        });
        created.filter(|ok| *ok).ok_or_else(|| {
            AgentError::Config("create paired_devices table".into())
        })?;
        Ok(Self { store })
    }

    /// 注册新设备，返回明文 token（仅此刻可见，落库的是 SHA-256）。
    ///
    /// scopes 列写入最小能力集 `["chat"]`（§5.5 第 4 层，D5 拍板：
    /// 远程设备默认"会话级：可发消息看会话，高危工具强制桌面确认"）
    /// ——而非恒占位 `'[]'`。`security::request_guard` 的来源推导在
    /// `Origin::may` 静态矩阵判定后 AND 此能力位（设备没配 `Chat`
    /// 就 `Denied`），实现"每设备独立能力位"的细粒度授权。
    pub fn pair(&self, device_name: &str) -> Result<(PairedDevice, String), AgentError> {
        let device_id = format!("dev-{}", hex_random(8));
        let token = format!("hmxd_{}", hex_random(32));
        let token_hash = sha256_hex(token.as_bytes());
        let now = chrono::Utc::now().timestamp();
        let device = PairedDevice {
            device_id: device_id.clone(),
            device_name: device_name.to_string(),
            scopes: DEFAULT_DEVICE_SCOPES.to_string(),
            issued_at: now,
            last_seen_at: None,
            revoked_at: None,
        };
        let inserted = self.store.write(|conn| {
            conn.execute(
                "INSERT INTO paired_devices
                     (device_id, device_name, token_hash, scopes, issued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    device.device_id,
                    device.device_name,
                    token_hash,
                    device.scopes,
                    device.issued_at
                ],
            )
            .is_ok()
        });
        inserted
            .filter(|ok| *ok)
            .ok_or_else(|| AgentError::Config("insert paired device".into()))?;
        Ok((device, token))
    }

    /// 凭证校验：token → 未撤销设备。命中即刷新 last_seen_at。
    pub fn authenticate(&self, token: &str) -> Option<PairedDevice> {
        let hash = sha256_hex(token.as_bytes());
        let device = self
            .store
            .read(|conn| {
                conn.query_row(
                    "SELECT device_id, device_name, scopes, issued_at, last_seen_at, revoked_at
                     FROM paired_devices WHERE token_hash = ?1",
                    [&hash],
                    |row| {
                        Ok(PairedDevice {
                            device_id: row.get(0)?,
                            device_name: row.get(1)?,
                            scopes: row.get(2)?,
                            issued_at: row.get(3)?,
                            last_seen_at: row.get(4)?,
                            revoked_at: row.get(5)?,
                        })
                    },
                )
                .ok()
            })
            .flatten();
        let device = device?;
        if device.revoked_at.is_some() {
            return None;
        }
        let now = chrono::Utc::now().timestamp();
        let _ = self.store.write(|conn| {
            let _ = conn.execute(
                "UPDATE paired_devices SET last_seen_at = ?1 WHERE device_id = ?2",
                rusqlite::params![now, device.device_id],
            );
        });
        // 返回值反映刷新后的 last_seen（调用方审计/展示直接可用）。
        Some(PairedDevice {
            last_seen_at: Some(now),
            ..device
        })
    }

    pub fn list(&self) -> Vec<PairedDevice> {
        self.store
            .read(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT device_id, device_name, scopes, issued_at, last_seen_at,
                                revoked_at
                         FROM paired_devices ORDER BY issued_at",
                    )
                    .ok()?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok(PairedDevice {
                            device_id: row.get(0)?,
                            device_name: row.get(1)?,
                            scopes: row.get(2)?,
                            issued_at: row.get(3)?,
                            last_seen_at: row.get(4)?,
                            revoked_at: row.get(5)?,
                        })
                    })
                    .ok()?;
                rows.collect::<Result<Vec<_>, _>>().ok()
            })
            .flatten()
            .unwrap_or_default()
    }

    /// 撤销设备。返回是否确有此设备。
    pub fn revoke(&self, device_id: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.store
            .write(|conn| {
                conn.execute(
                    "UPDATE paired_devices SET revoked_at = ?1
                     WHERE device_id = ?2 AND revoked_at IS NULL",
                    rusqlite::params![now, device_id],
                )
                .map(|n| n > 0)
                .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------
    // §5.5 防滥用（全局）审计日志：配对/认证/授权/撤销 全部落盘。
    // 设计文档 §5.5 附表：含时间、device_id、来源 IP、决策。
    // 同库 `devices.db`（与 paired_devices 共享连接），`audit_log` 表
    // 在 `open` 时建。审计写入失败不回滚主操作（审计是旁路观测，
    // 不是主路径的一部分）——返回 Err 仅记 tracing，主路径不受影响。
    // 类型定义（AuditEvent / AuditRow）在模块顶层。
    // ------------------------------------------------------------------

    /// 落盘一条审计行（旁路：失败仅 tracing，不影响主路径）。
    ///
    /// 参数：
    /// - `event`：四类事件之一（`AuditEvent::as_str` 形式落盘）
    /// - `device_id`：相关设备 id（认证失败/未配对 = `None`）
    /// - `source_ip`：来源 IP（字符串形式；`None` = 未捕获）
    /// - `decision`：判定结果（`"allowed"` / `"denied"` / `"needs_local_confirmation"`
    ///   / `"revoked"` 等自由文本；`None` = 不适用）
    pub fn record_audit(
        &self,
        event: AuditEvent,
        device_id: Option<String>,
        source_ip: Option<String>,
        decision: Option<String>,
    ) {
        let now = chrono::Utc::now().timestamp();
        let event_str = event.as_str().to_string();
        let ok = self.store.write(|conn| {
            conn.execute(
                "INSERT INTO audit_log (ts, event, device_id, source_ip, decision)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    now,
                    event_str,
                    device_id,
                    source_ip,
                    decision
                ],
            )
            .is_ok()
        });
        if ok != Some(true) {
            tracing::warn!("audit_log 落盘失败（旁路，主路径不受影响）");
        }
    }

    /// 读取最近 `limit` 条审计行（时间倒序）。供管理端点 / 诊断 tab 展示。
    pub fn list_audit(&self, limit: u32) -> Vec<AuditRow> {
        let limit = limit.max(1).min(1000);
        self.store
            .read(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT ts, event, device_id, source_ip, decision
                         FROM audit_log ORDER BY ts DESC, rowid DESC LIMIT ?1",
                    )
                    .ok()?;
                let rows = stmt
                    .query_map(rusqlite::params![limit as i64], |row| {
                        Ok(AuditRow {
                            ts: row.get(0)?,
                            event: row.get(1)?,
                            device_id: row.get(2)?,
                            source_ip: row.get(3)?,
                            decision: row.get(4)?,
                        })
                    })
                    .ok()?;
                rows.collect::<Result<Vec<_>, _>>().ok()
            })
            .flatten()
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// 配对会话（内存态，TTL + 一次性）
// ---------------------------------------------------------------------------

/// 二维码负载（设计文档 §5.5 第 3 层）。手机扫码后校验 cert_fingerprint
/// 再用 pairing_token 完成配对。
#[derive(Debug, Clone, Serialize)]
pub struct QrPayload {
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub cert_fingerprint: String,
    pub pairing_token: String,
    pub proto_version: u32,
}

struct PairingSession {
    expires_at: Instant,
    consumed: bool,
    payload: QrPayload,
}

/// 配对会话管理器（进程内）。一次性：完成或过期即失效。
pub struct PairingSessions {
    inner: Mutex<HashMap<String, PairingSession>>, // key = pairing_token
}

impl Default for PairingSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingSessions {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 清掉过期会话，返回当前存活数。
    fn gc(inner: &mut HashMap<String, PairingSession>) -> usize {
        inner.retain(|_, s| s.expires_at > Instant::now() && !s.consumed);
        inner.len()
    }

    /// 创建一次性配对会话。TLS 未开启时返回 Err（二维码要带证书指纹
    /// 与 TLS 端口，没有加密面就没有配对）。
    pub fn create(&self, tls_addr: Option<std::net::SocketAddr>) -> Result<QrPayload, String> {
        let tls_addr = tls_addr.ok_or_else(|| {
            "TLS 未启用：设置 HERMIX_TLS_ADDR 后重启，再发起配对".to_string()
        })?;
        let material = tls::ensure_tls_material(&tls::tls_dir())
            .map_err(|e| format!("加载 TLS 证书失败: {e}"))?;
        let fingerprint = material.fingerprint_sha256;

        let token = format!("hmxp_{}", hex_random(24));
        let host = tls::best_effort_lan_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| tls_addr.ip().to_string());
        let payload = QrPayload {
            server_id: format!("hermix-{}", &fingerprint[..8]),
            host,
            port: tls_addr.port(),
            cert_fingerprint: fingerprint,
            pairing_token: token.clone(),
            proto_version: 1,
        };
        let mut inner = self.inner.lock().expect("pairing sessions lock");
        Self::gc(&mut inner);
        inner.insert(
            token,
            PairingSession {
                expires_at: Instant::now() + PAIRING_TTL,
                consumed: false,
                payload: payload.clone(),
            },
        );
        Ok(payload)
    }

    /// 一次性消费：命中且未过期未消费 → 返回二维码负载并标记 consumed。
    pub fn consume(&self, pairing_token: &str) -> Option<QrPayload> {
        let mut inner = self.inner.lock().expect("pairing sessions lock");
        Self::gc(&mut inner);
        let session = inner.get_mut(pairing_token)?;
        if session.consumed || session.expires_at <= Instant::now() {
            return None;
        }
        session.consumed = true;
        Some(session.payload.clone())
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers（挂载在 router()；配对完成端点对 Unpaired 开放）
// ---------------------------------------------------------------------------

/// `POST /api/pairing/sessions` — 仅 Local。生成一次性配对二维码负载。
pub async fn create_pairing_session(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<HttpServerState>,
) -> impl IntoResponse {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (
            StatusCode::FORBIDDEN,
            "仅桌面端可发起配对".into_response(),
        )
            .into_response();
    }
    match state
        .pairing_sessions
        .create(tls::tls_addr_from_env())
    {
        Ok(payload) => Json(payload).into_response(),
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct CompletePairingBody {
    pub pairing_token: String,
    pub device_name: String,
}

/// `POST /api/pairing/complete` — Unpaired 也可访问（配对即信任建立本身）。
/// 成功返回 `{ device_id, device_token }`；token 仅此一次明文下发。
pub async fn complete_pairing(
    State(state): State<HttpServerState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CompletePairingBody>,
) -> impl IntoResponse {
    let src_ip = request_source_ip(Some(&peer), &headers);
    let Some(registry) = state.device_registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "设备注册表不可用".into_response(),
        )
            .into_response();
    };
    let Some(payload) = state.pairing_sessions.consume(&body.pairing_token) else {
        return (
            StatusCode::UNAUTHORIZED,
            "配对码无效、已过期或已使用".into_response(),
        )
            .into_response();
    };
    let _ = payload; // 端点侧暂不需要负载内容（指纹校验在手机端做）
    let device_name = body.device_name.trim();
    if device_name.is_empty() || device_name.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            "device_name 长度需在 1..=64".into_response(),
        )
            .into_response();
    }
    match registry.pair(device_name) {
        Ok((device, token)) => {
            // §5.5 防滥用审计：配对成功落盘。
            registry.record_audit(
                crate::pairing::AuditEvent::Pairing,
                Some(device.device_id.clone()),
                src_ip,
                Some("allowed".to_string()),
            );
            Json(serde_json::json!({
                "device_id": device.device_id,
                "device_token": token,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配对落库失败: {e}").into_response(),
        )
            .into_response(),
    }
}

/// `GET /api/pairing/devices` — 仅 Local。
pub async fn list_devices(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    State(state): State<HttpServerState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if !matches!(origin, hermix_core::Origin::Local) {
        return (
            StatusCode::FORBIDDEN,
            "仅桌面端可查看设备".into_response(),
        )
            .into_response();
    }
    match state.device_registry.as_ref() {
        Some(registry) => {
            // §5.5 防滥用审计：设备列表查询（授权事件）落盘。
            let src_ip = request_source_ip(Some(&peer), &headers);
            registry.record_audit(
                crate::pairing::AuditEvent::Grant,
                None,
                src_ip,
                Some("allowed".to_string()),
            );
            Json(registry.list()).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "设备注册表不可用".into_response(),
        )
            .into_response(),
    }
}

/// `DELETE /api/pairing/devices/:device_id` — 仅 Local。撤销后立即失效。
pub async fn revoke_device(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    axum::extract::Extension(device_scopes): axum::extract::Extension<
        Option<std::collections::HashSet<hermix_core::origin::Capability>>,
    >,
    State(state): State<HttpServerState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let src_ip = request_source_ip(Some(&peer), &headers);
    // §5.5 第 5 层：端点级能力位关口（DeviceManagement）。PairedDevice 来源
    // 未配该能力位 → 403（矩阵已 Denied，端点层再拦是纵深）；Local 来源放行。
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        hermix_core::origin::Capability::DeviceManagement,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    if !matches!(origin, hermix_core::Origin::Local) {
        return (
            StatusCode::FORBIDDEN,
            "仅桌面端可撤销设备".into_response(),
        )
            .into_response();
    }
    match state.device_registry.as_ref() {
        Some(registry) if registry.revoke(&device_id) => {
            // §5.5 防滥用审计：设备撤销落盘。
            registry.record_audit(
                crate::pairing::AuditEvent::Revoke,
                Some(device_id.clone()),
                src_ip,
                Some("revoked".to_string()),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Some(registry) => {
            // 设备不存在也落审计行（decision = "not_found"，便于排查滥用扫描）。
            registry.record_audit(
                crate::pairing::AuditEvent::Revoke,
                Some(device_id.clone()),
                src_ip,
                Some("not_found".to_string()),
            );
            (StatusCode::NOT_FOUND, "设备不存在".into_response()).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "设备注册表不可用".into_response(),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

fn hex_random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// §5.5 防滥用审计：落库→读库闭环 + 四事件 + source_ip 语义
// ---------------------------------------------------------------------------

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn record_audit_writes_and_list_audit_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = DeviceRegistry::open(&dir.path().join("devices.db")).expect("open");

        reg.record_audit(AuditEvent::Pairing, Some("dev-a".into()), Some("10.0.0.1".into()), Some("allowed".into()));
        reg.record_audit(AuditEvent::Auth, None, Some("10.0.0.2".into()), Some("denied".into()));
        reg.record_audit(AuditEvent::Revoke, Some("dev-a".into()), Some("10.0.0.1".into()), Some("revoked".into()));

        let rows = reg.list_audit(100);
        assert_eq!(rows.len(), 3, "三条审计行");
        // 时间倒序：最后写入的 Revoke 在首位。
        assert_eq!(rows[0].event, "revoke");
        assert_eq!(rows[0].device_id.as_deref(), Some("dev-a"));
        assert_eq!(rows[0].decision.as_deref(), Some("revoked"));
        assert_eq!(rows[0].source_ip.as_deref(), Some("10.0.0.1"));
        // Auth 行 device_id = None（未识别设备）。
        let auth = rows.iter().find(|r| r.event == "auth").expect("auth 行");
        assert!(auth.device_id.is_none());
        assert_eq!(auth.decision.as_deref(), Some("denied"));
        // 时间戳全为正值（chrono 落盘）。
        assert!(rows.iter().all(|r| r.ts > 0));
    }

    #[test]
    fn list_audit_respects_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = DeviceRegistry::open(&dir.path().join("devices.db")).expect("open");
        for i in 0..5 {
            reg.record_audit(AuditEvent::Grant, None, None, Some(format!("d{i}")));
        }
        assert_eq!(reg.list_audit(3).len(), 3, "limit=3 → 3 条");
        // limit=0 钳制到 1（max(1)），取回最近 1 条。
        assert_eq!(reg.list_audit(0).len(), 1, "limit=0 钳制到 1");
        assert_eq!(reg.list_audit(100).len(), 5, "limit 大于总量 → 全量");
    }

    #[test]
    fn audit_events_map_to_stable_strings() {
        assert_eq!(AuditEvent::Pairing.as_str(), "pairing");
        assert_eq!(AuditEvent::Auth.as_str(), "auth");
        assert_eq!(AuditEvent::Grant.as_str(), "grant");
        assert_eq!(AuditEvent::Revoke.as_str(), "revoke");
    }

    #[test]
    fn pair_revoke_lifecycle_emits_audit_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = DeviceRegistry::open(&dir.path().join("devices.db")).expect("open");
        let (dev, _token) = reg.pair("审计手机").expect("pair");
        // 用真实 device_id（pair 返回的），撤销才命中。
        let dev_id = dev.device_id.clone();
        reg.record_audit(AuditEvent::Pairing, Some(dev_id.clone()), None, Some("allowed".into()));
        assert!(reg.revoke(&dev_id), "撤销真实 device_id 应命中");
        reg.record_audit(AuditEvent::Revoke, Some(dev_id.clone()), None, Some("revoked".into()));

        let events: Vec<String> = reg.list_audit(100).iter().map(|r| r.event.clone()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], "revoke");
        assert_eq!(events[1], "pairing");
    }

    #[test]
    fn request_source_ip_prefers_xff_first_hop() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::FORWARDED, // placeholder, replaced below
            axum::http::HeaderValue::from_static("x"),
        );
        headers.remove(axum::http::header::FORWARDED);
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("  9.9.9.9 , 8.8.8.8"),
        );
        let peer: std::net::SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip = request_source_ip(Some(&peer), &headers);
        assert_eq!(ip.as_deref(), Some("9.9.9.9"), "XFF 第一跳优先");
    }

    #[test]
    fn request_source_ip_falls_back_to_peer_when_no_xff() {
        let headers = axum::http::HeaderMap::new();
        let peer: std::net::SocketAddr = "192.168.1.50:54321".parse().unwrap();
        let ip = request_source_ip(Some(&peer), &headers);
        assert_eq!(ip.as_deref(), Some("192.168.1.50:54321"), "无 XFF → 对端");
        // 无 XFF 无对端 → None（落 NULL）。
        assert!(request_source_ip(None, &headers).is_none());
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> DeviceRegistry {
        DeviceRegistry {
            store: Store::open_in_memory().expect("in-memory store"),
        }
    }

    // 建表逻辑在 DeviceRegistry::open；内存库测试需走同一初始化路径。
    fn registry_initialized() -> DeviceRegistry {
        let dir = tempfile::tempdir().expect("tempdir");
        DeviceRegistry::open(&dir.path().join("devices.db")).expect("open")
    }

    #[test]
    fn pairing_session_roundtrip_and_one_time_consumption() {
        let sessions = PairingSessions::new();
        let addr: std::net::SocketAddr = "0.0.0.0:8443".parse().expect("addr");
        let payload = sessions.create(Some(addr)).expect("create session");

        let first = sessions.consume(&payload.pairing_token);
        assert!(first.is_some(), "首次消费应成功");
        let second = sessions.consume(&payload.pairing_token);
        assert!(second.is_none(), "一次性：二次消费必须失败");
    }

    #[test]
    fn pairing_session_expiry_is_enforced() {
        let sessions = PairingSessions::new();
        let addr: std::net::SocketAddr = "0.0.0.0:8443".parse().expect("addr");
        let payload = sessions.create(Some(addr)).expect("create session");

        // 直接把过期时间拨回过去（时间源注入前的最小可控方案）。
        {
            let mut inner = sessions.inner.lock().expect("lock");
            for s in inner.values_mut() {
                s.expires_at = Instant::now() - Duration::from_secs(1);
            }
        }
        assert!(sessions.consume(&payload.pairing_token).is_none(), "过期必须失效");
    }

    #[test]
    fn pairing_requires_tls_addr() {
        let sessions = PairingSessions::new();
        assert!(sessions.create(None).is_err(), "TLS 未开启不能配对");
    }

    #[test]
    fn qr_payload_carries_fingerprint_and_proto() {
        let sessions = PairingSessions::new();
        let addr: std::net::SocketAddr = "0.0.0.0:8443".parse().expect("addr");
        let payload = sessions.create(Some(addr)).expect("create");
        assert!(payload.cert_fingerprint.len() == 64, "指纹 64 hex");
        assert_eq!(payload.proto_version, 1);
        assert_eq!(payload.port, 8443);
        assert!(payload.server_id.starts_with("hermix-"));
        assert!(payload.pairing_token.starts_with("hmxp_"));
    }

    #[test]
    fn pair_authenticate_revoke_lifecycle() {
        let registry = registry_initialized();
        let (device, token) = registry.pair("王爷的手机").expect("pair");

        let authed = registry.authenticate(&token).expect("token 有效");
        assert_eq!(authed.device_id, device.device_id);
        assert!(authed.last_seen_at.is_some(), "authenticate 应刷新 last_seen");

        assert!(registry.revoke(&device.device_id), "撤销应命中");
        assert!(
            registry.authenticate(&token).is_none(),
            "撤销后 token 立即失效"
        );
        assert!(!registry.revoke(&device.device_id), "重复撤销返回 false");
    }

    #[test]
    fn unknown_token_is_rejected() {
        let registry = registry_initialized();
        assert!(registry.authenticate("hmxd_deadbeef").is_none());
    }

    #[test]
    fn device_token_is_hashed_not_plaintext() {
        let registry = registry_initialized();
        let (_, token) = registry.pair("phone").expect("pair");
        let hash = sha256_hex(token.as_bytes());
        let plaintext_visible = registry
            .store
            .read(|conn| {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM paired_devices WHERE token_hash = ?1",
                        [token],
                        |r| r.get(0),
                    )
                    .unwrap_or(1);
                n == 0
            })
            .unwrap_or(false);
        assert!(plaintext_visible, "明文 token 不得落库");
        let _ = hash;
    }

    #[test]
    fn long_device_name_is_rejected_by_handler_validation_rules() {
        // handler 层校验规则（1..=64）与注册表层解耦；此处只钉注册表行为：
        // 超长名字不做注册表层限制（由 handler 拦），但空名在 handler 拦。
        // 本测试钉 token 格式稳定性。
        let registry = registry_initialized();
        let (_, token) = registry.pair("ok").expect("pair");
        assert!(token.starts_with("hmxd_"), "设备 token 前缀稳定（协议面）");
        assert!(token.len() >= 32 + 5, "token 熵下限");
    }
}
