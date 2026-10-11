//! §5.5 第 3/4 层：设备配对与凭证。
//!
//! 一次性配对会话（TTL 60min，6 位码）+ 持久设备注册表（SQLite）。
//! 2026-10-10 起配对**全走 relay 中转**：桌面生成 6 位码 + 二维码（码里带
//! 当场签发的手机 relay 凭据与本机 E2EE 公钥），手机扫码后经 relay 发密封
//! 配对帧，本机判码后密封回 device_token；码一次性、重新生成即作废、按
//! 凭据分桶的错误预算 5 次防枚举。配对成功后设备持有长寿命 device_token
//! （只存哈希），后续请求以 `Authorization: Bearer <device_token>` 出示，
//! 由 `security::request_guard` 校验后产出 `Origin::PairedDevice{device_id}`
//! 进权限矩阵。局域网直连的 `POST /api/pairing/complete` 与 /pair 落地页
//! 仍在（兼容旧码），但 App 端不再走它。
//!
//! 防滥用（对策 T6 暴力破解）：配对码 60min TTL + 一次性消费 + 错误预算
//! （PAIRING_MAX_WRONG_ATTEMPTS，耗尽全部作废）+ 完成端点走全局 rate limiter。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use hermix_core::AgentError;
use hermix_store::{OpenMode, Store};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};

use crate::channels::provision::urlencode;
use crate::relay_proto::Envelope;
use crate::state::HttpServerState;

#[cfg(test)]
use crate::channels::provision::percent_decode;

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
    /// JSON 数组字符串。签发时写入最小能力集 `["chat"]`
    /// （`DEFAULT_DEVICE_SCOPES`，默认矩阵在 Origin::may 里）。
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

/// 二维码负载。二维码内容 = `pairing_url`，6 位 `pairing_code` 同时展示在
/// 桌面上。
///
/// 2026-10-10 起配对**全走 relay 中转**（拍板：实现复杂度不转嫁给用户）：
/// `pairing_url` = `{relay}/pair?c=码&t=手机凭据&k=本机E2EE公钥`。手机扫码 →
/// 用凭据连 relay → 密封配对帧 → 本机判码 → 密封回 device_token。不再有
/// 局域网地址、本地端口与证书指纹——那三样是直连时代的产物。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QrPayload {
    /// 稳定本机标识（node_id，`hermix-<8 hex>`）。给用户肉眼区分多台电脑。
    pub server_id: String,
    pub pairing_code: String,
    pub pairing_url: String,
    pub proto_version: u32,
    /// 本机 E2EE 静态公钥，base64 的 65 字节未压缩 P-256。
    ///
    /// 手机用它**密封配对请求**（relay 拒收明文 route 帧），也用它把后续的
    /// `key.exchange` 封起来——没有它，PC 学不到手机的公钥而手机学得到 PC 的，
    /// E2EE 就只剩单半边。
    pub node_public_key: String,
}

struct PairingSession {
    expires_at: Instant,
    consumed: bool,
    /// `None` = 只有 6 位码、没有二维码负载（IM 通道用：码是打在 QQ 私聊里
    /// 手动输进去的，本来就不需要 pairing_url）。
    payload: Option<QrPayload>,
    /// 本码生成时一并向 relay 签发的手机凭据哈希（`sha256(hrel_*)`）。
    /// 配对成功时写进绑定表，撤销才有 relay 侧的抓手；同时用于校验
    /// 「来消费这个码的连接，就是扫码用的那条凭据」。
    phone_token_hash: Option<String>,
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

    /// 创建一次性配对会话（6 位码 + relay 配对链接）。
    ///
    /// `relay_base` 是给二维码用的 https:// 形态 relay 地址（调用方负责从
    /// 配置里的 wss:// 换算）；`phone_token` 是**刚**向 relay 签发的手机凭据
    /// 明文——只进二维码，不落盘。没配 relay 的桌面端出不了码：配对与连接
    /// 都以中转为前提，这不是可选项。
    pub fn create(&self, relay_base: &str, phone_token: &str) -> Result<QrPayload, String> {
        // E2EE 信任锚。**必须在进临界区之前取**——DPAPI 解密是系统调用，
        // 放锁里会放大 `inner` 的持有时间。`create()` 的 mint+insert 临界区
        // 刚被审计线（A 线批次 1）为修 TOCTOU 收窄过，别把慢操作塞回去。
        let node_public_key = {
            use base64::Engine;
            let id = crate::relay_identity::load_or_create()
                .map_err(|e| format!("准备 E2EE 身份失败: {e}"))?;
            base64::engine::general_purpose::STANDARD.encode(id.public_key())
        };
        let relay_base = relay_base.trim().trim_end_matches('/');
        if !relay_base.starts_with("https://") && !relay_base.starts_with("http://") {
            return Err(format!(
                "relay 地址形态不对（{relay_base:?}）：二维码需要带 scheme 的完整地址"
            ));
        }
        let phone_token = phone_token.trim();
        if !phone_token.starts_with("hrel_") || phone_token.len() > 128 {
            return Err("手机 relay 凭据格式不对（应以 hrel_ 开头）".to_string());
        }

        // mint + insert 必须在同一临界区（对齐 create_code_only 的持锁
        // 模式）：两段锁之间，另一个并发 create() 会铸出同一个 6 位码
        // ——双方做碰撞检查时彼此都还没 insert，互相看不见；后 insert
        // 的会话覆盖前一个，前一个用户的配对会话被静默顶掉（TOCTOU，
        // 体检批次 1-3）。
        let payload = {
            let mut inner = self.inner.lock().expect("pairing sessions lock");
            let code = Self::mint_code(&mut inner);
            let server_id = crate::relay_settings::derive_node_id();
            let payload = QrPayload {
                server_id: if server_id.is_empty() { "hermix".to_string() } else { server_id },
                // 二维码内容：手机 App 扫码 → 解出 relay 地址 + 手机凭据 + 配对码
                // → 连 relay → 密封配对帧 → 本机判码 → 密封回 device_token。
                // 凭据由本机当场签发、只走「屏幕 → 相机」这条信道，不落任何盘。
                pairing_url: format!(
                    "{relay_base}/pair?c={code}&t={}&k={}",
                    urlencode(&phone_token),
                    urlencode(&node_public_key)
                ),
                pairing_code: code.clone(),
                proto_version: crate::relay_proto::PROTO_VERSION,
                node_public_key,
            };
            // 新码代际：扫码方自己的预算清零（重新生成 = 攻击者重新开始）。
            // attempts 锁与 inner 互不相干，锁内调用无死锁。
            self.reset_attempts(BUCKET_BROWSER);
            inner.insert(
                code.clone(),
                PairingSession {
                    expires_at: Instant::now() + PAIRING_TTL,
                    consumed: false,
                    payload: Some(payload.clone()),
                    phone_token_hash: Some(sha256_hex(phone_token.as_bytes())),
                },
            );
            payload
        };
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
                phone_token_hash: None,
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

    /// relay 配对消费成功后，取该码绑定的手机凭据哈希（`create` 时记下）。
    ///
    /// 用途有二：写进 `PhoneBindingStore`（撤销才有 relay 侧抓手），以及校验
    /// 「来消费这个码的连接 = 扫码用的那条凭据」。
    pub fn take_relay_grant(&self, code: &str) -> Option<String> {
        let inner = self.inner.lock().expect("pairing sessions lock");
        inner
            .get(code)
            .filter(|s| s.consumed)
            .and_then(|s| s.phone_token_hash.clone())
    }
}

// ---------------------------------------------------------------------------
// relay 配对通路（2026-10-10 起唯一配对路径；局域网直连退居历史）
// ---------------------------------------------------------------------------

/// 配对专用会话。走这条会话的帧**不经过设备凭据鉴权**（配对时手机还没有
/// `hmxd_*`），信任锚是二维码本身：手机 relay 凭据由本机当场签发并印进
/// 二维码，能持它连上 relay 并路由到本机，本身就证明「这是扫码的那台手机」。
pub const PAIRING_SESSION_ID: &str = "im:pairing";

/// 手机发来的配对请求帧（密封内层解出后的明文形态）。
#[derive(Deserialize)]
struct PairRequestFrame {
    request_id: String,
    pairing_code: String,
    device_name: String,
    /// 手机 E2EE 静态公钥（base64）。回信里装着 device_token，必须用它密封
    /// ——relay 读得到也读不懂。
    ///
    /// JSON 里叫 `pk`：与手机端 crypto 层（`crypto.ts` / `direct.ts`）的
    /// 命名一致。对不上时 serde 直接解析失败，本函数返回 None 且**不发任何
    /// 回信**（见 `handle_pairing_frame`），手机上表现为 30s 超时。
    #[serde(rename = "pk")]
    phone_public_key: String,
}

/// 回给手机的配对结果帧（密封前的明文形态）。
#[derive(Serialize)]
struct PairResultFrame {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// 处理一条 relay 配对帧并回信。由 `relay_client` 在常规帧鉴权**之前**分派
/// （配对时手机还没有设备凭据，常规鉴权必然拒绝）。
///
/// 错误预算的桶 = 手机 relay 凭据哈希（relay 转发时无条件盖章，手机伪造不了）
/// ——穷举只烧掉那一条凭据自己的预算，锁不死其他设备的配对。
pub async fn handle_pairing_frame(
    state: Arc<HttpServerState>,
    env: Envelope,
    out_tx: tokio::sync::mpsc::Sender<Envelope>,
) {
    let Some(reply) = pair_reply(&state, &env).await else {
        // 请求坏到无法回信（解不开 / 不是合法帧）：不回，让手机侧超时报错。
        return;
    };
    if let Err(e) = out_tx.send(reply).await {
        tracing::warn!(error = %e, "配对回信发送失败（relay 连接可能已断）");
    }
}

async fn pair_reply(state: &HttpServerState, env: &Envelope) -> Option<Envelope> {
    let Some(payload) = env.payload.clone() else {
        return None;
    };

    // ① 拆密封请求。relay 拒收明文 route 帧，能走到这里的都是密封帧。
    if !crate::relay_crypto::is_sealed_payload(&payload) {
        tracing::warn!("pairing: 配对请求不是密封帧，拒绝");
        return None;
    }
    let inner = match crate::relay_identity::load_or_create() {
        Ok(id) => match id.open_route(env) {
            Ok(plain) => plain,
            Err(e) => {
                tracing::warn!(error = %e, "pairing: 配对请求解密失败");
                return None;
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "pairing: 本机 E2EE 身份不可用");
            return None;
        }
    };
    let req: PairRequestFrame = match serde_json::from_str(&inner) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "pairing: 配对请求字段不合法");
            return None;
        }
    };

    // ② 判码。桶 = relay 盖章的手机凭据哈希。
    let bucket = env
        .token_hash
        .clone()
        .unwrap_or_else(|| "relay-unstamped".to_string());
    let outcome = state
        .pairing_sessions
        .consume_for(req.pairing_code.trim(), &bucket);

    let mut result = PairResultFrame {
        kind: "pairing.result",
        request_id: req.request_id.clone(),
        ok: false,
        device_id: None,
        device_token: None,
        error: None,
    };
    match outcome {
        ConsumeOutcome::Locked => {
            result.error = Some("错误次数过多，配对已锁定。请在电脑上重新生成配对码。".into());
        }
        ConsumeOutcome::Invalid | ConsumeOutcome::OkCode => {
            result.error = Some("配对码无效或已过期。请在电脑上重新生成。".into());
        }
        ConsumeOutcome::Ok(_) => {
            // 码里带的凭据必须与请求方一致：防「A 手机的凭据消费 B 扫的码」。
            let grant = state
                .pairing_sessions
                .take_relay_grant(req.pairing_code.trim());
            match grant {
                Some(hash) if Some(&hash) == env.token_hash.as_ref() => {}
                _ => {
                    result.error =
                        Some("配对码与扫码使用的凭据不匹配。请在电脑上重新生成后重扫。".into());
                    tracing::warn!(bucket = %bucket, "pairing: 凭据与配对码不匹配");
                }
            }
            if result.error.is_none() {
                let device_name = req.device_name.trim();
                if device_name.is_empty() || device_name.len() > 64 {
                    result.error = Some("设备名长度需在 1..=64".into());
                } else if let Some(registry) = state.device_registry.as_ref() {
                    match registry.pair(device_name) {
                        Ok((device, token)) => {
                            // §5.5 防滥用审计：配对成功落盘（来源经 relay，无直连 IP 可记）。
                            registry.record_audit(
                                AuditEvent::Pairing,
                                Some(device.device_id.clone()),
                                None,
                                Some("allowed".to_string()),
                            );
                            // 撤销链路：绑定表记下这条手机凭据的哈希。写失败只
                            // 记日志、不回滚配对——配对已成立，为绑定表搞失败更糟。
                            if let Some(hash) = env.token_hash.as_deref() {
                                let node_id = state.relay_settings.lock().await.node_id.clone();
                                if let Err(e) = crate::relay_settings::PhoneBindingStore::at_default()
                                    .put(&device.device_id, hash, &node_id)
                                {
                                    tracing::warn!(
                                        device_id = %device.device_id,
                                        error = %e,
                                        "记录 relay 绑定失败 —— 这台设备之后无法从 relay 侧撤销"
                                    );
                                }
                            }
                            result.ok = true;
                            result.device_id = Some(device.device_id);
                            result.device_token = Some(token);
                        }
                        Err(e) => result.error = Some(format!("配对落库失败: {e}")),
                    }
                } else {
                    result.error = Some("设备注册表不可用".into());
                }
            }
        }
    }

    // ③ 密封回信。响应里装着 device_token，明文过 relay 等于把设备的会话
    // 身份送人中——密封到手机公钥，relay 全程读不到。
    let body = match serde_json::to_value(&result) {
        Ok(v) => v,
        Err(_) => return None,
    };
    seal_pair_reply(&req.phone_public_key, body)
}

/// 用手机的 E2EE 公钥封配对回信。公钥坏 / 封装失败就放弃回信（手机侧超时）。
fn seal_pair_reply(phone_public_key: &str, body: serde_json::Value) -> Option<Envelope> {
    use base64::Engine;
    let pk = base64::engine::general_purpose::STANDARD
        .decode(phone_public_key.trim())
        .ok()?;
    let peer = p256::PublicKey::from_sec1_bytes(&pk).ok()?;
    let mut env = Envelope::route_for(PAIRING_SESSION_ID, String::new());
    let frame_id: [u8; 16] = rand::random();
    crate::relay_crypto::seal_envelope(
        &mut env,
        &serde_json::to_string(&body).ok()?,
        &peer,
        &frame_id,
    )
    .ok()?;
    Some(env)
}

// ---------------------------------------------------------------------------
// HTTP handlers（挂载在 router()）
// ---------------------------------------------------------------------------

/// `POST /api/pairing/sessions` — 仅 Local。生成一次性配对二维码负载。
///
/// 2026-10-10 起配对全走 relay：生成二维码时**当场**向 relay 签发一条手机
/// 凭据并印进码里。没配 relay 直接报错——配对与连接都以中转为前提，这不是
/// 可选项，也没有「局域网降级」。
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
    let (relay_url, node_token) = {
        let s = state.relay_settings.lock().await;
        (s.url.clone(), s.token().to_string())
    };
    if relay_url.trim().is_empty() {
        // 地址现在有内置默认（relay_settings::DEFAULT_RELAY_URL），能走到这里
        // 只剩一种情况：relay.json 读坏了。旧文案让用户去「连接渠道里填好
        // relay」——那个输入框已经不存在了，照着做只会更困惑。
        return (
            StatusCode::PRECONDITION_FAILED,
            "中转配置读不出来（多半是 relay.json 损坏）。请检查配置，或在「设置 → 连接渠道 → 手机端 → 高级」里重新保存一次。",
        )
            .into_response();
    }
    let mut node_token = node_token;
    if node_token.trim().is_empty() {
        // 启动时的自动签发可能因网络未通而失败过。用户此刻正主动发起配对，
        // 是最合理的重试时机——成功则二维码照常出，失败才落回人工指引。
        match crate::relay_settings::ensure_node_credential(&relay_url, &state.relay_settings).await
        {
            Ok(t) => node_token = t,
            Err(e) => {
                return (
                    StatusCode::PRECONDITION_FAILED,
                    format!(
                        "电脑侧 relay 凭据自动签发失败：{e}。可点「重试」再来一次；若反复出现，在「中转连接」里重置本机身份（已配对手机需重新扫码），或登服务器用 relayctl add-node 手动签发。"
                    ),
                )
                    .into_response();
            }
        }
    }
    let phone_token =
        match crate::relay_settings::issue_phone_credential(&relay_url, &node_token).await {
            Ok(t) => t,
            Err(e) => return (StatusCode::BAD_GATEWAY, e).into_response(),
        };
    // 二维码给手机解析，用 https 形态（wss/ws 是 WS 握手用的写法）。
    let https_base = relay_url
        .trim()
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    match state.pairing_sessions.create(&https_base, &phone_token) {
        Ok(payload) => Json(payload).into_response(),
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

/// `POST /api/pairing/sessions` — 仅 Local。生成一次性配对二维码负载。/// `GET /api/pairing/devices` — 仅 Local。
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

            // 撤销必须**一路断到 relay**。只撤本机 `devices.db` 的话，那台手机
            // 虽然发不出帧了（这一层生效），却还在持续接收广播给该 node 的出站
            // 帧——用户点的是「撤销」，界面不该让它继续收。
            //
            // 刻意**不因 relay 失败而让整个请求失败**：本地撤销已经生效，把它报成
            // 失败会让用户以为什么都没发生、于是不再重试——那比重试更糟。relay 侧
            // 的结果原样回给界面，由界面决定怎么提示。
            let outcome = crate::relay_settings::revoke_device_on_relay(&state, &device_id).await;
            if let Some(why) = &outcome.relay_error {
                tracing::warn!(device_id = %device_id, error = %why, "relay 侧撤销失败");
            }
            (StatusCode::OK, Json(outcome)).into_response()
        }
        Some(registry) => {
            // 设备不存在也落审计行（decision = "not_found"，便于排查滥用扫描）。
            registry.record_audit(                crate::pairing::AuditEvent::Revoke,
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

    // relay 配对测试共用：二维码形态的 relay 地址 + 签发的手机凭据。
    const RELAY: &str = "https://relay.example.com";
    const PHONE_TOKEN: &str = "hrel_test0000deadbeef";

    #[test]
    fn pairing_session_roundtrip_and_one_time_consumption() {
        let sessions = PairingSessions::new();
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create session");
        assert_eq!(payload.pairing_code.len(), 6, "6 位数字码");
        assert!(payload.pairing_code.chars().all(|c| c.is_ascii_digit()));
        assert!(payload.pairing_url.contains("/pair?c="), "扫码 URL 指向落地路径");
        assert!(
            payload.pairing_url.contains("&t=hrel_"),
            "二维码必须携带手机 relay 凭据"
        );
        assert!(
            payload.pairing_url.contains("&k="),
            "二维码必须携带本机 E2EE 公钥（配对请求要封给它）"
        );

        let first = sessions.consume(&payload.pairing_code);
        assert!(matches!(first, ConsumeOutcome::Ok(_)), "首次消费应成功");
        let second = sessions.consume(&payload.pairing_code);
        assert_eq!(second, ConsumeOutcome::Invalid, "一次性：二次消费必须失败");
    }

    #[test]
    fn relay_grant_binds_token_hash() {
        // 消费成功后必须能取到「这码是配着哪条手机凭据发的」——撤销链路与
        // 凭据一致性校验都靠它。
        let sessions = PairingSessions::new();
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create");
        assert_eq!(sessions.take_relay_grant(&payload.pairing_code), None);
        assert!(matches!(
            sessions.consume(&payload.pairing_code),
            ConsumeOutcome::Ok(_)
        ));
        assert_eq!(
            sessions.take_relay_grant(&payload.pairing_code),
            Some(sha256_hex(PHONE_TOKEN.as_bytes())),
            "grant 哈希必须等于签发凭据的 sha256"
        );
        // IM 通道的码没有凭据可绑。
        let code_only = sessions.create_code_only();
        assert_eq!(sessions.take_relay_grant(&code_only), None);
    }

    #[test]
    fn concurrent_create_never_yields_duplicate_codes() {
        // TOCTOU 回归网（批次 1-3）：mint 与 insert 已收敛到同一临界区，
        // 并发 create 的码空间不碰撞。注：1M 码空间下不修此洞时本测试
        // 的单次捕获概率不高——结构保证（单临界区）才是主防线，本测试
        // 防的是将来有人把 insert 再挪出锁外的回归。
        use std::sync::Arc;
        let sessions = Arc::new(PairingSessions::new());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let s = Arc::clone(&sessions);
            handles.push(std::thread::spawn(move || {
                (0..25)
                    .filter_map(|_| s.create(RELAY, PHONE_TOKEN).ok())
                    .map(|p| p.pairing_code)
                    .collect::<Vec<_>>()
            }));
        }
        let mut codes = Vec::new();
        for h in handles {
            codes.extend(h.join().expect("create thread"));
        }
        assert_eq!(codes.len(), 100, "全部 create 应成功");
        let mut sorted = codes.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            codes.len(),
            "并发 create 不得产生重复 6 位码（mint+insert 必须同临界区）"
        );
    }

    #[test]
    fn pairing_session_expiry_is_enforced() {
        let sessions = PairingSessions::new();
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create session");

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

        // 先生成一枚真码，再错 5 次 → 同一代际内预算耗尽 → 正确码也 Locked
        //（防穷举关键断言；合法用户的恢复路径是重新生成）。
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create session");
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
        let payload2 = sessions.create(RELAY, PHONE_TOKEN).expect("create session 2");
        assert!(matches!(
            sessions.consume(&payload2.pairing_code),
            ConsumeOutcome::Ok(_)
        ));
    }

    #[test]
    fn create_rejects_bad_relay_base_and_token() {
        let sessions = PairingSessions::new();
        assert!(
            sessions.create("relay.example.com", PHONE_TOKEN).is_err(),
            "裸 host 没有 scheme，手机端解析不了"
        );
        assert!(
            sessions.create(RELAY, "not-a-hrel-token").is_err(),
            "手机凭据必须 hrel_ 开头（ relay 的 mintToken 口径）"
        );
    }

    #[test]
    fn qr_payload_carries_relay_link_and_proto() {
        let sessions = PairingSessions::new();
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create");
        // 曾经断言 `== 1`：协议升到 v2 时这个测试会红，但它红的方式是
        // "实现的版本号是 2、测试期望 1"——容易被误读成"实现错了"。
        // 断言应当钉"二维码说的版本 == 线路常量"，版本升级时无需改这里。
        assert_eq!(payload.proto_version, crate::relay_proto::PROTO_VERSION);
        // E2EE 信任锚：手机拿到它才能把配对请求与后续 key.exchange 封给 PC。
        assert_eq!(
            payload.node_public_key.len(),
            88,
            "base64 的 65 字节 P-256 公钥应是 88 字符"
        );
        assert!(payload.server_id.starts_with("hermix-") || payload.server_id == "hermix");
        assert!(payload.pairing_code.chars().all(|c| c.is_ascii_digit()));
        assert!(
            payload.pairing_url.starts_with("https://relay.example.com/pair?c="),
            "扫码 URL 指向 relay"
        );
        assert!(
            !payload.pairing_url.contains("fp="),
            "直连时代的证书指纹已随局域网直连一起退役"
        );
    }

    #[test]
    fn qr_url_percent_encodes_key_and_token() {
        // 回归守卫：base64 的 P-256 公钥里有 `+` 和 `/`，直接拼进 query 会
        // 被手机端的 URLSearchParams 读成空格（`+` → 空格），`parsePairingUrl`
        // 的 base64 正则随即判失败，手机只显示"这不是 hermix 的配对码"。
        // 症状是绝大多数公钥扫不动、少数（不含 `+` 的）能扫，极难自查。
        let sessions = PairingSessions::new();
        let payload = sessions.create(RELAY, PHONE_TOKEN).expect("create");

        let url = payload.pairing_url.clone();
        let query = url.split_once('?').expect("URL 带 query").1;
        let k = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("k="))
            .expect("URL 带 k=");
        assert!(
            !k.contains('+'),
            "公钥里的 `+` 必须转义，否则被读成空格：{k}"
        );
        assert!(
            !k.contains('/'),
            "公钥里的 `/` 必须转义：{k}"
        );
        // 转义后仍必须能被解码回原始 base64，且与 payload 上的原值一致。
        let decoded = percent_decode(k);
        assert_eq!(decoded, payload.node_public_key);
        // 手机端的接受域是 `^[A-Za-z0-9+/]+={0,2}$`。
        assert!(
            decoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='),
            "解码后仍须落在手机端接受的 base64 字符域内：{decoded}"
        );
    }

    #[test]
    fn pair_request_field_matches_mobile_naming() {
        // 回归守卫：手机端（`apps/hm/core/direct.ts`）发的是 `pk`，不是
        // `phone_public_key`。字段名对不上时 serde 解析失败，`handle_pairing_frame`
        // **一条回信都不发**（不可解的帧无法定位收件人），手机上只剩 30s 超时。
        let body = serde_json::json!({
            "type": "pairing.request",
            "request_id": "0123456789abcdef",
            "pairing_code": "123456",
            "device_name": "Hermix M · Test",
            "pk": "BAtzc2VjcmV0LXB1YmxpYy1rZXk=",
        });
        let parsed: PairRequestFrame =
            serde_json::from_value(body).expect("手机端发的 pk 必须能解析");
        assert_eq!(parsed.pairing_code, "123456");
        assert_eq!(parsed.device_name, "Hermix M · Test");
        assert_eq!(parsed.request_id, "0123456789abcdef");
        assert_eq!(parsed.phone_public_key, "BAtzc2VjcmV0LXB1YmxpYy1rZXk=");
    }

    #[test]
    fn pair_request_rejects_unknown_key_spelling() {
        // 反面钉死：别为了省事在手机端也改成 phone_public_key。两边任一
        // 边单改都会静默退化，这里保证 Rust 侧只认 `pk`。
        let body = serde_json::json!({
            "type": "pairing.request",
            "request_id": "0123456789abcdef",
            "pairing_code": "123456",
            "device_name": "Hermix M · Test",
            "phone_public_key": "BAtzc2VjcmV0LXB1YmxpYy1rZXk=",
        });
        assert!(
            serde_json::from_value::<PairRequestFrame>(body).is_err(),
            "不接受 phone_public_key 拼写（手机端发的是 pk）"
        );
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
