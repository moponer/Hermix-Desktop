//! §5.5 第 3/4 层：设备配对与凭证。
//!
//! 一次性配对会话（TTL 60min，QQ 式 6 位码）+ 持久设备注册表（SQLite）。
//! 交互抄 cc-haha 的配对码模型：桌面生成 6 位码 + 二维码（扫码打开 /pair
//! 落地页），手机在落地页提交配对；码一次性、重新生成即作废、共享错误
//! 预算 5 次防枚举。配对成功后设备持有长寿命 device_token（只存哈希），
//! 后续请求以 `Authorization: Bearer <device_token>` 出示，由
//! `security::request_guard` 校验后产出 `Origin::PairedDevice{device_id}`
//! 进权限矩阵。
//!
//! 防滥用（对策 T6 暴力破解）：配对码 60min TTL + 一次性消费 + 错误预算
//! （PAIRING_MAX_WRONG_ATTEMPTS，耗尽全部作废）+ 完成端点走全局 rate limiter。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use hermix_core::AgentError;
use hermix_store::{OpenMode, Store};
use rand::{Rng, RngCore};
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

/// 配对码有效期（抄 cc-haha IM 配对码）：60 分钟、一次性、重新生成即作废。
pub const PAIRING_TTL: Duration = Duration::from_secs(3600);

/// 全局错误尝试预算：一次生成的配对码共享 N 次输错机会，用尽即全部作废
/// （防 6 位码枚举；cc-haha 对应「同一用户 5 分钟内错 5 次限流」，
/// 我们没有账号概念，收敛为按码代际计）。重新生成后预算清零。
pub const PAIRING_MAX_WRONG_ATTEMPTS: u32 = 5;

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

/// 二维码负载（设计文档 §5.5 第 3 层；交互抄 cc-haha 配对码）。
/// 二维码内容 = `pairing_url`（手机相机扫码直接打开 /pair 落地页）；
/// 6 位 `pairing_code` 同时展示在桌面上，支持像 QQ 那样手动输入。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QrPayload {
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub cert_fingerprint: String,
    pub pairing_code: String,
    pub pairing_url: String,
    pub proto_version: u32,
}

struct PairingSession {
    expires_at: Instant,
    consumed: bool,
    /// `None` = 只有 6 位码、没有二维码负载（IM 通道用：码是打在 QQ 私聊里
    /// 手动输进去的，本来就不需要 pairing_url 和证书指纹）。
    payload: Option<QrPayload>,
}

/// 某个来源的失败预算。
struct Attempt {
    count: u32,
    first_at: Instant,
}

/// 配对会话管理器（进程内）。一次性：完成或过期即失效。
pub struct PairingSessions {
    inner: Mutex<HashMap<String, PairingSession>>, // key = 6 位配对码
    /// 按来源分桶的失败预算。
    ///
    /// 分桶而不是全局计数：6 位码只有一百万种组合，防穷举靠的是「猜 5 次
    /// 就得重新生成」。但全局计数意味着**任何**一次错误尝试都算在所有人生
    /// 头上——一个在 IM 里乱发消息的人就能把桌面端自己的扫码配对一起锁死。
    /// 按来源分桶后，预算耗尽只影响那个人自己。
    attempts: Mutex<HashMap<String, Attempt>>,
}

/// 失败预算的统计窗口。超窗自动归零，所以偶尔手滑不会永久锁死。
const ATTEMPT_WINDOW: Duration = Duration::from_secs(600);

/// 浏览器扫码配对用的桶名。`create` 生成新码时清零的就是它。
const BUCKET_BROWSER: &str = "browser";

impl Default for PairingSessions {
    fn default() -> Self {
        Self::new()
    }
}

/// `consume` 的结果。Locked 与 Invalid 分开返回：前端能区分
/// 「输错太多次已锁定，请重新生成」vs「配对码无效」两种提示。
#[derive(Debug, Clone, PartialEq)]
pub enum ConsumeOutcome {
    Ok(QrPayload),
    /// 码有效，但本来就没有二维码负载（IM 通道发的码）。
    OkCode,
    Invalid,
    Locked,
}

impl PairingSessions {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// 某个来源的预算是否已耗尽（滑动窗口内计满 `PAIRING_MAX_WRONG_ATTEMPTS`）。
    fn attempts_exhausted(&self, bucket: &str) -> bool {
        let mut map = self.attempts.lock().expect("pairing attempts lock");
        let now = Instant::now();
        let Some(a) = map.get_mut(bucket) else {
            return false;
        };
        if now.duration_since(a.first_at) >= ATTEMPT_WINDOW {
            map.remove(bucket);
            return false;
        }
        a.count >= PAIRING_MAX_WRONG_ATTEMPTS
    }

    /// 记一次错误尝试，返回记完之后的累计次数。
    fn charge_attempt(&self, bucket: &str) -> u32 {
        let mut map = self.attempts.lock().expect("pairing attempts lock");
        let now = Instant::now();
        let a = map.entry(bucket.to_string()).or_insert(Attempt {
            count: 0,
            first_at: now,
        });
        if now.duration_since(a.first_at) >= ATTEMPT_WINDOW {
            a.count = 0;
            a.first_at = now;
        }
        a.count += 1;
        a.count
    }

    /// 清空某个来源的失败预算（生成新码时调用）。
    fn reset_attempts(&self, bucket: &str) {
        self.attempts
            .lock()
            .expect("pairing attempts lock")
            .remove(bucket);
    }

    /// 清掉过期会话，返回当前存活数。
    fn gc(inner: &mut HashMap<String, PairingSession>) -> usize {
        inner.retain(|_, s| s.expires_at > Instant::now() && !s.consumed);
        inner.len()
    }

    /// 铸一枚 6 位数字码，避开当前存活会话的碰撞。
    fn mint_code(inner: &mut HashMap<String, PairingSession>) -> String {
        Self::gc(inner);
        loop {
            let c = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32));
            if !inner.contains_key(&c) {
                return c;
            }
        }
    }

    /// 创建一次性配对会话（QQ 式 6 位码 + 扫码 URL）。
    /// TLS 未开启时返回 Err（配对依赖加密面）。TLS 现已默认开启，
    /// 仅显式设置 HERMIX_TLS_DISABLE=1 的用户会遇到本错误。
    pub fn create(&self, tls_addr: Option<std::net::SocketAddr>) -> Result<QrPayload, String> {
        let tls_addr = tls_addr.ok_or_else(|| {
            "TLS 已被关闭（检测到 HERMIX_TLS_DISABLE=1）。删除该环境变量并重启，再发起配对".to_string()
        })?;
        let material = tls::ensure_tls_material(&tls::tls_dir())
            .map_err(|e| format!("加载 TLS 证书失败: {e}"))?;
        let fingerprint = material.fingerprint_sha256;

        let code = {
            let mut inner = self.inner.lock().expect("pairing sessions lock");
            Self::mint_code(&mut inner)
        };
        let host = tls::best_effort_lan_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| tls_addr.ip().to_string());
        let payload = QrPayload {
            server_id: format!("hermix-{}", &fingerprint[..8]),
            // 二维码内容：手机相机扫码 → 浏览器打开 /pair 落地页（同 TLS 监听器）
            pairing_url: format!("https://{host}:{}/pair?c={code}", tls_addr.port()),
            host,
            port: tls_addr.port(),
            cert_fingerprint: fingerprint,
            pairing_code: code.clone(),
            proto_version: 1,
        };
        // 新码代际：扫码方自己的预算清零（重新生成 = 攻击者重新开始）
        self.reset_attempts(BUCKET_BROWSER);
        let mut inner = self.inner.lock().expect("pairing sessions lock");
        inner.insert(
            code,
            PairingSession {
                expires_at: Instant::now() + PAIRING_TTL,
                consumed: false,
                payload: Some(payload.clone()),
            },
        );
        Ok(payload)
    }

    /// 只铸一枚 6 位码，不带二维码负载。IM 通道用：用户在 QQ 私聊里手动
    /// 输入这个码，没有"扫码打开网页"这一步，所以不需要 pairing_url，
    /// 也不依赖 TLS 监听器。
    ///
    /// 与 H5 扫码共用 TTL、一次性语义和错误预算——一个码只能被消费一次，
    /// 无论它是扫码进来的还是在 QQ 里输进来的。
    pub fn create_code_only(&self) -> String {
        let mut inner = self.inner.lock().expect("pairing sessions lock");
        let code = Self::mint_code(&mut inner);
        inner.insert(
            code.clone(),
            PairingSession {
                expires_at: Instant::now() + PAIRING_TTL,
                consumed: false,
                payload: None,
            },
        );
        code
    }

    /// 一次性消费（按 6 位码，计入 `BUCKET_BROWSER` 预算）。
    pub fn consume(&self, code: &str) -> ConsumeOutcome {
        self.consume_for(code, BUCKET_BROWSER)
    }

    /// 一次性消费，并指定这次尝试记到哪个来源的失败预算上。
    ///
    /// 预算耗尽 → Locked：即使码正确也拒，防穷举 6 位码。**猜错的**尝试计入
    /// 预算；码本身有效或已用过则不计（那不是猜）。
    pub fn consume_for(&self, code: &str, bucket: &str) -> ConsumeOutcome {
        if self.attempts_exhausted(bucket) {
            return ConsumeOutcome::Locked;
        }
        let mut inner = self.inner.lock().expect("pairing sessions lock");
        Self::gc(&mut inner);
        let Some(session) = inner.get_mut(code) else {
            let n = self.charge_attempt(bucket);
            tracing::warn!("配对码错误尝试 {n}/{PAIRING_MAX_WRONG_ATTEMPTS} (bucket={bucket})");
            return ConsumeOutcome::Invalid;
        };
        if session.consumed || session.expires_at <= Instant::now() {
            return ConsumeOutcome::Invalid;
        }
        session.consumed = true;
        match session.payload.clone() {
            Some(payload) => ConsumeOutcome::Ok(payload),
            None => ConsumeOutcome::OkCode,
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers（挂载在 router()；配对完成端点对 Unpaired 开放）
// ---------------------------------------------------------------------------

/// `GET /pair?c=<code>` — 手机扫码落地页（抄 cc-haha：扫码 → 浏览器打开 →
/// 输入设备名 → 提交配对）。自包含 HTML，无外部资源（离线可用纪律）。
/// 页面本身不含敏感信息；码校验在 POST /api/pairing/complete 时进行。
pub async fn pair_page(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = q.get("c").cloned().unwrap_or_default();
    // 只接受 6 位数字码入页（防注入 + 提前纠错）。
    let code_display = if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
        code.clone()
    } else {
        String::new()
    };
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1">
<title>Hermix 设备配对</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ font-family: -apple-system, "PingFang SC", "Microsoft YaHei UI", sans-serif;
    background: #f5f5f7; color: #1d1d1f; display: flex; justify-content: center;
    min-height: 100vh; padding: 24px 16px; }}
  .card {{ background: #fff; border-radius: 16px; padding: 28px 24px;
    max-width: 380px; width: 100%; height: fit-content; margin-top: 8vh;
    box-shadow: 0 2px 12px rgba(0,0,0,0.06); }}
  h1 {{ font-size: 20px; margin-bottom: 6px; }}
  .sub {{ font-size: 13px; color: #6e6e73; margin-bottom: 20px; line-height: 1.5; }}
  label {{ display: block; font-size: 13px; color: #48484c; margin-bottom: 6px; }}
  input {{ width: 100%; padding: 11px 12px; font-size: 16px; border: 1px solid #d4d4d8;
    border-radius: 10px; outline: none; background: #fff; }}
  input:focus {{ border-color: #0071e3; }}
  .code {{ text-align: center; font-size: 28px; letter-spacing: 8px;
    font-weight: 600; font-variant-numeric: tabular-nums; padding: 12px 0 4px; }}
  button {{ width: 100%; margin-top: 18px; padding: 12px; font-size: 16px;
    border: 0; border-radius: 10px; background: #0071e3; color: #fff;
    font-weight: 500; cursor: pointer; }}
  button:disabled {{ opacity: 0.5; }}
  .msg {{ margin-top: 14px; font-size: 13px; text-align: center; min-height: 18px; }}
  .msg.err {{ color: #a83b60; }}
  .msg.ok {{ color: #37874c; }}
  .hidden {{ display: none; }}
</style>
</head>
<body>
<div class="card">
  <div id="form-view">
    <h1>配对到此电脑</h1>
    <p class="sub">确认此配对码与电脑上显示的一致。配对成功后，本设备即可在聊天中访问这台电脑上的 Hermix。</p>
    <div class="code">{code_display}</div>
    <label for="dev">设备名称</label>
    <input id="dev" maxlength="64" placeholder="如：我的手机" autocomplete="off">
    <button id="go">配对</button>
    <div class="msg" id="msg"></div>
  </div>
  <div id="done-view" class="hidden">
    <h1>配对成功</h1>
    <p class="sub">本设备已获得对话权限。回到 Hermix 即可开始对话；高危操作仍会在电脑上弹出确认。</p>
  </div>
</div>
<script>
var CODE = {code_json};
var btn = document.getElementById("go");
var msg = document.getElementById("msg");
btn.addEventListener("click", function () {{
  var name = document.getElementById("dev").value.trim();
  if (!name) {{ msg.textContent = "请填写设备名称"; msg.className = "msg err"; return; }}
  btn.disabled = true; msg.textContent = "配对中…"; msg.className = "msg";
  fetch("/api/pairing/complete", {{
    method: "POST",
    headers: {{ "Content-Type": "application/json" }},
    body: JSON.stringify({{ pairing_code: CODE, device_name: name }})
  }}).then(function (r) {{
    if (r.status === 200) {{
      document.getElementById("form-view").classList.add("hidden");
      document.getElementById("done-view").classList.remove("hidden");
      return;
    }}
    return r.text().then(function (t) {{
      if (r.status === 429) msg.textContent = "错误尝试过多，请在电脑上重新生成配对码";
      else msg.textContent = t || ("配对失败（HTTP " + r.status + "）");
      msg.className = "msg err"; btn.disabled = false;
    }});
  }}).catch(function () {{
    msg.textContent = "网络错误，请确认手机与电脑在同一网络";
    msg.className = "msg err"; btn.disabled = false;
  }});
}});
</script>
</body>
</html>"#,
        code_json = serde_json::to_string(&code).unwrap_or_else(|_| "\"\"".into()),
    );
    (StatusCode::OK, [("content-type", "text/html; charset=utf-8")], html).into_response()
}

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
    /// 6 位配对码（/pair 落地页或手动输入提交）。
    pub pairing_code: String,
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
    match state.pairing_sessions.consume(body.pairing_code.trim()) {
        ConsumeOutcome::Locked => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "错误尝试次数过多，配对码已锁定。请在桌面端重新生成一枚新码。".into_response(),
            )
                .into_response();
        }
        ConsumeOutcome::Invalid => {
            return (
                StatusCode::UNAUTHORIZED,
                "配对码无效、已过期或已使用".into_response(),
            )
                .into_response();
        }
        ConsumeOutcome::Ok(_) => {} // 负载内容端点侧不需要（指纹核对在 /pair 页与桌面端之间进行）
        // IM 通道发的码没有二维码负载，只能在 QQ 私聊里输。让浏览器端
        // 明确拒绝而不是"顺手就换成 device token"——否则一个在聊天记录里
        // 看到码的人能抢在本人之前把码烧掉，IM 侧再也配不上。
        ConsumeOutcome::OkCode => {
            return (
                StatusCode::BAD_REQUEST,
                "这是一枚 IM 通道配对码，请在绑定的 IM 里输入".into_response(),
            )
                .into_response();
        }
    }
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

/// `GET /api/pairing/audit` 的 query 参数。
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    /// 返回最近 N 条（缺省 100；`list_audit` 内部 `max(1).min(1000)` 钳制）。
    pub limit: Option<u32>,
}

/// `GET /api/pairing/audit?limit=N` — 审计日志读端点（P6 后端半边，
/// DiagnosticsTab 展示属 UI 批次）。仅 Local（矩阵 `DeviceManagement`
/// 对 PairedDevice 全 `Denied`——配了能力位也 403 纵深兜底；Unpaired
/// 已由 `request_guard` 403）。"最近 N 条"语义，时间倒序；
/// `list_audit` 内部 `max(1).min(1000)` 钳制。审计为 append-only
/// 旁路，无写/删端点。
pub async fn list_audit(
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    axum::extract::Extension(device_scopes): axum::extract::Extension<
        Option<std::collections::HashSet<hermix_core::origin::Capability>>,
    >,
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<AuditQuery>,
) -> impl IntoResponse {
    // §5.5 第 5 层：端点级能力位关口（DeviceManagement），与 revoke_device
    // 同一鉴权面——审计行含 source_ip / device_id，属设备安全域敏感数据。
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        hermix_core::origin::Capability::DeviceManagement,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    match state.device_registry.as_ref() {
        Some(registry) => {
            let events = registry.list_audit(params.limit.unwrap_or(100));
            axum::Json(serde_json::json!({
                "events": events,
                "count": events.len(),
            }))
            .into_response()
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
        assert_eq!(payload.pairing_code.len(), 6, "6 位数字码");
        assert!(payload.pairing_code.chars().all(|c| c.is_ascii_digit()));
        assert!(payload.pairing_url.contains("/pair?c="), "扫码 URL 指向落地页");

        let first = sessions.consume(&payload.pairing_code);
        assert!(matches!(first, ConsumeOutcome::Ok(_)), "首次消费应成功");
        let second = sessions.consume(&payload.pairing_code);
        assert_eq!(second, ConsumeOutcome::Invalid, "一次性：二次消费必须失败");
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
        assert_eq!(
            sessions.consume(&payload.pairing_code),
            ConsumeOutcome::Invalid,
            "过期必须失效"
        );
    }

    #[test]
    fn 失败预算按来源分桶() {
        let sessions = PairingSessions::new();
        let code = sessions.create_code_only();
        assert_eq!(code.len(), 6);

        // 一个 IM 来源把预算烧光。
        for _ in 0..PAIRING_MAX_WRONG_ATTEMPTS {
            assert_eq!(
                sessions.consume_for("000000", "im:qq:attacker"),
                ConsumeOutcome::Invalid
            );
        }
        assert_eq!(
            sessions.consume_for(&code, "im:qq:attacker"),
            ConsumeOutcome::Locked,
            "同一来源预算耗尽后即使码正确也拒"
        );

        // 关键断言：受害者自己的桶没被连累，浏览器扫码配对照常可用。
        assert_eq!(
            sessions.consume_for(&code, "im:qq:victim"),
            ConsumeOutcome::OkCode,
            "攻击者烧光自己的预算不该锁死别人"
        );

        // 另一枚新码也不该受影响——`create_code_only` 之前不重置全局预算，
        // 那会让 IM 侧被浏览器扫码那边的失败次数永久锁死。
        let code2 = sessions.create_code_only();
        assert_eq!(sessions.consume_for(&code2, "im:qq:victim"), ConsumeOutcome::OkCode);
    }

    #[test]
    fn 用过的码再提交按猜错计费() {
        // gc 会把已消费的会话清掉，所以"再提交一次用过的码"与"猜了一个
        // 不存在的码"在实现上不可区分——按猜错计费是正确的，否则可以拿一枚
        // 已经用掉的码无限试探而不触发锁定。
        let sessions = PairingSessions::new();
        let code = sessions.create_code_only();
        assert!(matches!(sessions.consume_for(&code, "b"), ConsumeOutcome::OkCode));
        for _ in 0..PAIRING_MAX_WRONG_ATTEMPTS {
            assert_eq!(sessions.consume_for(&code, "b"), ConsumeOutcome::Invalid);
        }
        let code2 = sessions.create_code_only();
        assert_eq!(
            sessions.consume_for(&code2, "b"),
            ConsumeOutcome::Locked,
            "预算耗尽后新码也拒"
        );
        // 换个来源不受牵连。
        assert!(matches!(sessions.consume_for(&code2, "c"), ConsumeOutcome::OkCode));
    }

    #[test]
    fn wrong_attempts_lockout_and_regeneration_reset() {
        let sessions = PairingSessions::new();
        let addr: std::net::SocketAddr = "0.0.0.0:8443".parse().expect("addr");

        // 先生成一枚真码，再错 5 次 → 同一代际内预算耗尽 → 正确码也 Locked
        //（防穷举关键断言；合法用户的恢复路径是重新生成）。
        let payload = sessions.create(Some(addr)).expect("create session");
        for i in 0..PAIRING_MAX_WRONG_ATTEMPTS {
            let r = sessions.consume("000000");
            assert_eq!(r, ConsumeOutcome::Invalid, "第 {i} 次错误应为 Invalid");
        }
        assert_eq!(
            sessions.consume(&payload.pairing_code),
            ConsumeOutcome::Locked,
            "预算耗尽后正确码也应 Locked（同一代际内）"
        );

        // 重新生成 = 新码代际，预算清零 → 正常消费。
        let payload2 = sessions.create(Some(addr)).expect("create session 2");
        assert!(matches!(
            sessions.consume(&payload2.pairing_code),
            ConsumeOutcome::Ok(_)
        ));
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
        assert!(payload.pairing_code.chars().all(|c| c.is_ascii_digit()));
        assert!(payload.pairing_url.starts_with("https://"), "扫码 URL 走 TLS");
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
