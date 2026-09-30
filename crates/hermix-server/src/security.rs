//! Optional Bearer API key, IP allowlist, and per-IP rate limiting for `hermes-server`.
//!
//! Environment:
//! - `HERMES_HTTP_API_KEY` — if set, require `Authorization: Bearer <key>` for all routes except `/health`, and `/metrics` unless `HERMES_HTTP_METRICS_REQUIRE_AUTH=1`.
//! - `HERMES_HTTP_ALLOWED_IPS` — comma-separated client IPs (e.g. `127.0.0.1,::1`). When non-empty, only these IPs may access routes other than `/health` (metrics follow the same rule unless exempt below).
//! - `HERMES_HTTP_RATE_LIMIT_PER_MINUTE` — max requests per client IP per rolling 60s window (0 = disabled).
//! - `HERMES_HTTP_METRICS_REQUIRE_AUTH` — set to `1` to protect `/metrics` with the same Bearer rules.
//!
//! Policy admin routes (`/v1/policy/*`):
//! - `HERMES_POLICY_ADMIN_TOKEN` — preferred shared secret; mutating routes require header `X-Hermes-Policy-Admin: <same value>` (same env var for CLI `/policy`).
//! - `HERMES_HTTP_POLICY_ADMIN_KEY` — legacy alias for the same secret if `HERMES_POLICY_ADMIN_TOKEN` is unset.
//! - `HERMES_HTTP_POLICY_REQUIRE_ADMIN=1` — reject policy mutations with 503 unless an admin secret is configured (see above).
//! - `HERMES_HTTP_POLICY_ALLOWED_ACTORS` — comma-separated allowlist for JSON field `actor` on policy mutations; when non-empty, unknown actors get 403.
//! - `HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN=1` — require `X-Hermes-Policy-Admin` on `GET /v1/policy/export` (requires admin secret env to be set).

use std::collections::HashMap;
use hermix_config::env_compat;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use hermix_core::Origin;
use hermix_core::origin::{Capability, Decision};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct HttpSecurity {
    pub api_key: Option<Arc<str>>,
    pub metrics_require_auth: bool,
    pub rate_limit_per_minute: u32,
    /// When `Some` and non-empty, client IP must be in this set (not applied to `/health`).
    pub allowed_ips: Option<Vec<IpAddr>>,
    /// 请求签名 nonce 缓存（§5.5 第 4 层，对策 T4）。进程内单次使用。
    pub nonce_cache: crate::request_signature::NonceCache,
    /// 暴力破解 IP 锁定（§5.5 对策 T6）。`None` = 未启用（默认启用）。
    /// 按 source IP 累计认证失败次数，窗口内超阈值即锁定，冷却期内
    /// 该 IP 所有请求直接 429 拒绝（防 brute-force 撞 token/api_key）。
    pub auth_fail_lockout: Option<Arc<AuthFailLockout>>,
}

/// 暴力破解 IP 锁定（§5.5 对策 T6，headless 可验的纯内存结构）。
///
/// 仿 [`RateLimiter`] 的 `Arc<Mutex<HashMap<String, Vec<Instant>>>>` +
/// 时间窗模式（Hermix 是单机桌面应用，无跨进程需求，进程内语义足够；
/// 跨重启持久化锁定需落库，属另一批次）。
///
/// 语义（按 source IP）：
/// - `record_failure(ip)`：窗口内失败次数 +1；达到 `threshold` 时把
///   `locked_until` 设为 `now + cooldown`。
/// - `record_success(ip)`：清零该 IP 的失败计数（合法用户输错一次后
///   输对了不该累积锁定）。
/// - `is_locked(ip)`：`locked_until` 在未来 → 锁定中（返回 429）；
///   过期自动解锁（无显式解锁调用，冷却到期即放行）。
///
/// 失败计数用 `Vec<Instant>`（窗口内时间戳列表），`record_failure` 时
/// 先 `retain` 掉窗外条目再 push——窗口内连续 `threshold` 次失败才
/// 触发锁定，而非"终身累计"。
#[derive(Clone)]
pub struct AuthFailLockout {
    /// 按 IP 的失败时间戳 + 锁定截止时间。
    inner: Arc<Mutex<HashMap<String, (Vec<Instant>, Option<Instant>)>>>,
    /// 触发锁定的窗口内失败次数阈值（默认 5）。
    threshold: u32,
    /// 失败计数窗口（默认 10 分钟）。
    window: Duration,
    /// 锁定期（默认 15 分钟）。
    cooldown: Duration,
}

impl AuthFailLockout {
    pub fn new(threshold: u32, window_secs: u64, cooldown_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            threshold,
            window: Duration::from_secs(window_secs),
            cooldown: Duration::from_secs(cooldown_secs),
        }
    }

    /// 该 IP 当前是否处于锁定冷却期。
    pub async fn is_locked(&self, ip: &str) -> bool {
        let g = self.inner.lock().await;
        g.get(ip)
            .and_then(|(_, until)| *until)
            .map(|until| until > Instant::now())
            .unwrap_or(false)
    }

    /// 记录一次认证失败。窗口内失败次数达到阈值时锁定该 IP。
    pub async fn record_failure(&self, ip: &str) {
        let mut g = self.inner.lock().await;
        let now = Instant::now();
        let entry = g.entry(ip.to_string()).or_insert((Vec::new(), None));
        // 清掉窗口外的失败时间戳。
        entry.0.retain(|t| now.duration_since(*t) < self.window);
        // 仍在冷却期内 → 有界续期（攻击持续则锁定延长，防止"刚好过
        // 冷却就重来"的绕锁；但从原截止时间起算封顶 now+2*cooldown，
        // 避免滑动窗口导致攻击者永不解锁）。
        if let Some(until) = entry.1 {
            if until > now {
                let extended = until + self.cooldown;
                let cap = now + self.cooldown + self.cooldown;
                entry.1 = Some(std::cmp::min(extended, cap));
                return;
            }
        }
        entry.0.push(now);
        if entry.0.len() as u32 >= self.threshold {
            entry.1 = Some(now + self.cooldown);
        }
    }

    /// 记录一次认证成功。清零该 IP 的失败计数与锁定（合法用户输错后
    /// 输对了不该继续累积）。
    pub async fn record_success(&self, ip: &str) {
        if let Some(entry) = self.inner.lock().await.get_mut(ip) {
            entry.0.clear();
            entry.1 = None;
        }
    }
}

/// Guards policy lifecycle HTTP APIs (admin token + actor allowlist).
#[derive(Clone, Default)]
pub struct PolicyGuardConfig {
    /// When set, mutating routes require `X-Hermes-Policy-Admin` matching this value.
    pub admin_key: Option<Arc<str>>,
    /// When true and `admin_key` is missing, policy mutations return 503.
    pub require_admin: bool,
    /// When non-empty, JSON `actor` must be one of these (trimmed, exact match).
    pub allowed_actors: Vec<String>,
    /// When true, `GET /v1/policy/export` requires a valid `X-Hermes-Policy-Admin` header.
    pub export_require_admin: bool,
}

impl PolicyGuardConfig {
    pub fn from_env() -> Self {
        let admin_key = env_compat("HERMIX_POLICY_ADMIN_TOKEN", "HERMES_POLICY_ADMIN_TOKEN")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                env_compat("HERMIX_HTTP_POLICY_ADMIN_KEY", "HERMES_HTTP_POLICY_ADMIN_KEY")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .map(|s| Arc::from(s.into_boxed_str()));
        let require_admin = env_compat("HERMIX_HTTP_POLICY_REQUIRE_ADMIN", "HERMES_HTTP_POLICY_REQUIRE_ADMIN")
            .as_deref()
            == Some("1");
        let export_require_admin = env_compat("HERMIX_HTTP_POLICY_EXPORT_REQUIRE_ADMIN", "HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN")
            .as_deref()
            == Some("1");
        let allowed_actors: Vec<String> = env_compat("HERMIX_HTTP_POLICY_ALLOWED_ACTORS", "HERMES_HTTP_POLICY_ALLOWED_ACTORS")
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            admin_key,
            require_admin,
            allowed_actors,
            export_require_admin,
        }
    }

    /// Validate `actor` for policy mutation bodies.
    pub fn check_actor(&self, actor: &str) -> Result<(), &'static str> {
        let actor = actor.trim();
        if actor.is_empty() {
            return Err("actor must be non-empty");
        }
        if !self.allowed_actors.is_empty() && !self.allowed_actors.iter().any(|a| a == actor) {
            return Err("actor not in HERMES_HTTP_POLICY_ALLOWED_ACTORS");
        }
        Ok(())
    }

    /// Require admin header for mutating policy routes.
    pub fn check_mutation_admin(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        if self.require_admin && self.admin_key.is_none() {
            return Err("HERMES_HTTP_POLICY_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty");
        }
        if let Some(ref key) = self.admin_key {
            let got = header_policy_admin(headers);
            if got != Some(key.as_ref()) {
                return Err("missing or invalid X-Hermes-Policy-Admin");
            }
        }
        Ok(())
    }

    /// Optional admin for read-only export.
    pub fn check_export_admin(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        if !self.export_require_admin {
            return Ok(());
        }
        let Some(ref key) = self.admin_key else {
            return Err("HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty");
        };
        let got = header_policy_admin(headers);
        if got != Some(key.as_ref()) {
            return Err("missing or invalid X-Hermes-Policy-Admin");
        }
        Ok(())
    }
}

fn header_policy_admin(headers: &HeaderMap) -> Option<&str> {
    const NAME: &str = "x-hermes-policy-admin";
    headers
        .get(NAME)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl HttpSecurity {
    pub fn from_env() -> Self {
        let api_key = env_compat("HERMIX_HTTP_API_KEY", "HERMES_HTTP_API_KEY")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| Arc::from(s.into_boxed_str()));
        let metrics_require_auth = env_compat("HERMIX_HTTP_METRICS_REQUIRE_AUTH", "HERMES_HTTP_METRICS_REQUIRE_AUTH")
            .as_deref()
            == Some("1");
        let rate_limit_per_minute = env_compat("HERMIX_HTTP_RATE_LIMIT_PER_MINUTE", "HERMES_HTTP_RATE_LIMIT_PER_MINUTE")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let allowed_ips = parse_allowed_ips_from_env();
        // §5.5 对策 T6：暴力破解 IP 锁定。默认启用（防 brute-force 撞
        // token/api_key），HERMIX_AUTH_FAIL_LOCKOUT=0 可关。阈值 / 窗口 /
        // 冷却走 env 覆盖，缺省 5 次 / 600s / 900s。
        let lockout_enabled = env_compat("HERMIX_AUTH_FAIL_LOCKOUT", "HERMES_AUTH_FAIL_LOCKOUT")
            .as_deref()
            != Some("0");
        let lockout_threshold: u32 = env_compat("HERMIX_AUTH_FAIL_THRESHOLD", "HERMES_AUTH_FAIL_THRESHOLD")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        let lockout_window_secs: u64 =
            env_compat("HERMIX_AUTH_FAIL_WINDOW_SECS", "HERMES_AUTH_FAIL_WINDOW_SECS")
                .and_then(|s| s.parse().ok())
                .unwrap_or(600);
        let lockout_cooldown_secs: u64 =
            env_compat("HERMIX_AUTH_FAIL_COOLDOWN_SECS", "HERMES_AUTH_FAIL_COOLDOWN_SECS")
                .and_then(|s| s.parse().ok())
                .unwrap_or(900);
        let auth_fail_lockout = if lockout_enabled {
            Some(Arc::new(AuthFailLockout::new(
                lockout_threshold,
                lockout_window_secs,
                lockout_cooldown_secs,
            )))
        } else {
            None
        };
        Self {
            api_key,
            metrics_require_auth,
            rate_limit_per_minute,
            allowed_ips,
            nonce_cache: crate::request_signature::NonceCache::new(),
            auth_fail_lockout,
        }
    }
}

fn parse_allowed_ips_from_env() -> Option<Vec<IpAddr>> {
    let raw = env_compat("HERMIX_HTTP_ALLOWED_IPS", "HERMES_HTTP_ALLOWED_IPS")?;
    let ips = parse_allowed_ips(&raw);
    if ips.is_empty() {
        None
    } else {
        Some(ips)
    }
}

/// Parse a comma-separated IP list (for tests and tooling).
pub fn parse_allowed_ips(raw: &str) -> Vec<IpAddr> {
    raw.split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect()
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    per_minute: u32,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            per_minute,
        }
    }

    pub async fn allow(&self, key: String) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let mut g = self.inner.lock().await;
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let v = g.entry(key).or_default();
        v.retain(|t| now.duration_since(*t) < window);
        if v.len() as u32 >= self.per_minute {
            return false;
        }
        v.push(now);
        true
    }
}

fn client_key(req: &Request<Body>) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn query_bearer_token(req: &Request<Body>) -> Option<String> {
    req.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let mut iter = pair.splitn(2, '=');
            let key = iter.next()?;
            let value = iter.next().unwrap_or_default();
            if key == "token" && !value.trim().is_empty() {
                Some(value.to_string())
            } else {
                None
            }
        })
    })
}

fn client_ip(req: &Request<Body>) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

fn ip_allowed(security: &HttpSecurity, req: &Request<Body>) -> bool {
    let Some(ref allow) = security.allowed_ips else {
        return true;
    };
    if allow.is_empty() {
        return true;
    }
    let Some(ip) = client_ip(req) else {
        return false;
    };
    allow.contains(&ip)
}

/// 来源推导（§5.5 第 5 层）。优先级：设备凭证 > loopback > 其他。
/// 来源由服务端凭证判定，客户端无法声明（对策 T8）。
fn derive_origin(
    device: Option<&crate::pairing::PairedDevice>,
    ip: Option<IpAddr>,
) -> Origin {
    match device {
        Some(d) => Origin::PairedDevice {
            device_id: d.device_id.clone(),
        },
        None => match ip {
            Some(ip) if ip.is_loopback() => Origin::Local,
            _ => Origin::Unpaired,
        },
    }
}

/// 能力位解析（§5.5 第 4 层细粒度授权）：`paired_devices.scopes`
/// 是 JSON 数组字符串（`["chat"]` 形式，`pairing::DEFAULT_DEVICE_SCOPES`
/// 写的最小集；`[]` = 未配任何能力位，全拒绝）。映射到
/// `hermix_core::origin::Capability` 枚举——解析失败的项忽略
/// （保守侧：未知能力位不授予，矩阵仍兜底）。
fn parse_device_scopes(scopes: &str) -> std::collections::HashSet<Capability> {
    let mut set = std::collections::HashSet::new();
    for token in serde_json::from_str::<Vec<String>>(scopes)
        .into_iter()
        .flatten()
    {
        let cap = match token.to_ascii_lowercase().as_str() {
            "chat" => Capability::Chat,
            "approvealways" => Capability::ApproveAlways,
            "dangeroustool" => Capability::DangerousTool,
            "configmutation" => Capability::ConfigMutation,
            "devicemanagement" => Capability::DeviceManagement,
            "workbenchaccess" => Capability::WorkbenchAccess,
            _ => continue,
        };
        set.insert(cap);
    }
    set
}

/// §5.5 第 4 层细粒度授权判定：设备能力位 ∩ 静态权限矩阵。
///
/// 语义：矩阵（`Origin::may`）给出"该来源**最多**能做什么"；设备
/// 能力位（`paired_devices.scopes`）给出"该设备**实际**被授予什么"。
/// 取交集——矩阵 `Denied` 的（授权/配置/管理/工作台）即使能力位误配
/// 也保持 `Denied`（纵深防御）；矩阵 `Allowed`/`NeedsLocalConfirmation`
/// 的，能力位未配即降为 `Denied`。非 PairedDevice 来源（Local /
/// Unpaired）无能力位维度，直接透传矩阵判定（Local 矩阵全 Allowed、
/// Unpaired 矩阵全 Denied，不受能力位影响）。
fn effective_decision_for_paired(
    origin: &Origin,
    cap: Capability,
    device_scopes: Option<&crate::pairing::PairedDevice>,
) -> Decision {
    let matrix = origin.may(cap);
    let Some(device) = device_scopes else {
        return matrix;
    };
    if matrix == Decision::Denied {
        return Decision::Denied;
    }
    if parse_device_scopes(&device.scopes).contains(&cap) {
        matrix
    } else {
        Decision::Denied
    }
}

/// 端点级能力位判定（§5.5 第 5 层 HTTP 端点消费点）。直接吃
/// `request_guard` 装进 Extension 的 `HashSet<Capability>`（设备能力
/// 位集），而非 `PairedDevice` 字符串——省掉一次 `parse_device_scopes`。
/// 语义同 [`effective_decision_for_paired`]：矩阵 `Denied` 保持
/// `Denied`（纵深防御）；矩阵 `Allowed`/`NeedsLocalConfirmation` 的，
/// 设备能力位未配该 Capability 即降为 `Denied`；非 PairedDevice
/// 来源（`device_scopes = None`）直接透传矩阵判定。
pub fn effective_decision_with_scopes(
    origin: &Origin,
    cap: Capability,
    device_scopes: Option<&std::collections::HashSet<Capability>>,
) -> Decision {
    let matrix = origin.may(cap);
    let Some(set) = device_scopes else {
        return matrix;
    };
    if matrix == Decision::Denied {
        return Decision::Denied;
    }
    if set.contains(&cap) {
        matrix
    } else {
        Decision::Denied
    }
}

/// Unpaired 纵深防御关口（对策 T1）：仅放行配对完成端点与 /pair 扫码
/// 落地页（信任建立本身）。/health 与 /metrics 在更早分支已返回。
fn unpaired_gate_allows(path: &str) -> bool {
    path == "/api/pairing/complete" || path == "/pair"
}

pub async fn request_guard(
    security: Arc<HttpSecurity>,
    rate: Arc<RateLimiter>,
    devices: Option<Arc<crate::pairing::DeviceRegistry>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    if req.method() == axum::http::Method::OPTIONS {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if path == "/health" {
        return next.run(req).await;
    }
    if path == "/metrics" && !security.metrics_require_auth {
        if !ip_allowed(&security, &req) {
            hermix_telemetry::record_http_reject();
            return (StatusCode::FORBIDDEN, "client IP not in allowlist").into_response();
        }
        return next.run(req).await;
    }

    if !ip_allowed(&security, &req) {
        hermix_telemetry::record_http_reject();
        return (StatusCode::FORBIDDEN, "client IP not in allowlist").into_response();
    }

    // §5.5 对策 T6：暴力破解 IP 锁定前置检查。被锁定的 source IP 直接
    // 429 拒绝（不评估 token/api_key，避免攻击者借失败响应试探）。
    // 锁定状态挂在 `security.auth_fail_lockout`（进程内，仿 RateLimiter）。
    let ck = client_key(&req);
    if let Some(lockout) = &security.auth_fail_lockout {
        if lockout.is_locked(&ck).await {
            hermix_telemetry::record_http_reject();
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "too many auth failures from this IP, temporarily locked",
            )
                .into_response();
        }
    }

    // Bearer 凭证双轨：api_key（桌面/网关既有凭证）或已配对设备 token。
    // 命中设备 token 产出 PairedDevice 来源（§5.5 第 4 层）。
    let header_token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(str::trim));
    let token = header_token
        .map(std::string::ToString::to_string)
        .or_else(|| query_bearer_token(&req));
    let device = token
        .as_deref()
        .and_then(|t| devices.as_ref().and_then(|r| r.authenticate(t)));

    // §5.5 防滥用审计：凭证认证命中落盘（含 device_id + 来源 IP + 决策）。
    // 认证失败（token 无效/撤销）→ decision = "denied"；成功 → "allowed"。
    // 审计是旁路观测：失败仅 tracing，不阻断主路径。
    if let Some(reg) = devices.as_ref() {
        let src_ip = client_key(&req);
        if let Some(d) = &device {
            reg.record_audit(
                crate::pairing::AuditEvent::Auth,
                Some(d.device_id.clone()),
                Some(src_ip.clone()),
                Some("allowed".to_string()),
            );
        } else if token.is_some() {
            // 出示了 token 但未命中（无效/撤销）→ 审计 rejected。
            // device_id = None（未识别设备），decision = "denied"。
            reg.record_audit(
                crate::pairing::AuditEvent::Auth,
                None,
                Some(src_ip.clone()),
                Some("denied".to_string()),
            );
        }
    }

    // §5.5 第 4 层请求签名（对策 T4）：已配对设备必须在 Bearer 之外附
    // 有效签名头（时间窗 + nonce 单次使用），token 泄漏 ≠ 无限使用。
    // 纯 Bearer（桌面端 api_key / 旧客户端）不受影响。
    if device.is_some() {
        let token_str = token.as_deref().unwrap_or_default();
        let sig_ok = crate::request_signature::signature_header(req.headers())
            .and_then(|h| {
                crate::request_signature::verify_signature(
                    h,
                    token_str,
                    req.method().as_str(),
                    path,
                    chrono::Utc::now().timestamp(),
                    &security.nonce_cache,
                )
                .ok()
            })
            .is_some();
        if !sig_ok {
            hermix_telemetry::record_http_reject();
            // §5.5 对策 T6：认证失败计数（锁定冷却期内该 IP 后续请求 429）。
            if let Some(lockout) = &security.auth_fail_lockout {
                lockout.record_failure(&ck).await;
            }
            return (
                StatusCode::UNAUTHORIZED,
                "valid X-Hermix-Signature required for paired device",
            )
                .into_response();
        }
    }

    if let Some(key) = security.api_key.as_ref() {
        let api_key_ok = token.as_deref() == Some(key.as_ref());
        if !api_key_ok && device.is_none() {
            hermix_telemetry::record_http_reject();
            // §5.5 对策 T6：认证失败计数。
            if let Some(lockout) = &security.auth_fail_lockout {
                lockout.record_failure(&ck).await;
            }
            return (
                StatusCode::UNAUTHORIZED,
                "missing or invalid Authorization: Bearer token",
            )
                .into_response();
        }
    }

    // §5.5 对策 T6：认证成功（api_key 命中 / 设备 token 命中）清零该 IP
    // 的失败计数——合法用户输错一次后输对了不该继续累积锁定。
    if security.auth_fail_lockout.is_some() {
        if device.is_some()
            || token.as_deref() == security.api_key.as_deref().map(|k| k.as_ref())
        {
            if let Some(lockout) = &security.auth_fail_lockout {
                lockout.record_success(&ck).await;
            }
        }
    }

    if !rate.allow(ck).await {
        hermix_telemetry::record_http_reject();
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    // §5.5 第 5 层：来源推导 + Unpaired 关口。
    let origin = derive_origin(device.as_ref(), client_ip(&req));
    if matches!(origin, Origin::Unpaired) && !unpaired_gate_allows(path) {
        hermix_telemetry::record_http_reject();
        return (
            StatusCode::FORBIDDEN,
            "unpaired origin: only /health and pairing endpoints are available",
        )
            .into_response();
    }

    // §5.5 第 4 层细粒度授权：把本次请求命中设备的 `paired_devices.scopes`
    // 能力位集装进 Extension，与 `origin` 并列。下游（http 端点 / 工具层
    // 关口）按 `origin` 取设备 id，再查此集判定"矩阵 ∩ 能力位"。
    // Local / Unpaired 来源 = None（矩阵自身已是最宽/最严，无能力位维度）。
    let device_scopes: Option<std::collections::HashSet<Capability>> =
        match &device {
            Some(d) if matches!(origin, Origin::PairedDevice { .. }) => {
                Some(parse_device_scopes(&d.scopes))
            }
            _ => None,
        };

    let (mut parts, body) = req.into_parts();
    parts.extensions.insert(origin);
    parts.extensions.insert(device_scopes);
    let req = Request::from_parts(parts, body);

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parse_allowed_ips_trims() {
        let v = parse_allowed_ips(" 127.0.0.1 , ::1 ");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"127.0.0.1".parse().unwrap()));
        assert!(v.contains(&"::1".parse().unwrap()));
    }

    // ----- §5.5 第 3/4 层接线：设备 token → PairedDevice 来源 -----

    #[test]
    fn device_token_yields_paired_device_origin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry =
            crate::pairing::DeviceRegistry::open(&dir.path().join("devices.db"))
                .expect("registry");
        let (_device, token) = registry.pair("测试手机").expect("pair");

        let authed = registry.authenticate(&token).expect("token 有效");
        let origin = derive_origin(Some(&authed), Some([192, 168, 1, 9].into()));
        match origin {
            Origin::PairedDevice { device_id } => {
                assert_eq!(device_id, _device.device_id);
            }
            other => panic!("设备 token 应产出 PairedDevice，got {other:?}"),
        }
    }

    #[test]
    fn unpaired_gate_blocks_non_pairing_paths() {
        // 远程无凭证 = Unpaired。
        assert!(matches!(
            derive_origin(None, Some([192, 168, 1, 50].into())),
            Origin::Unpaired
        ));
        // 关口只放配对完成端点。
        assert!(unpaired_gate_allows("/api/pairing/complete"));
        assert!(!unpaired_gate_allows("/v1/sessions"));
        assert!(!unpaired_gate_allows("/api/config"));
        // 回环无凭证 = Local（桌面自身），不受关口约束。
        assert!(matches!(
            derive_origin(None, Some([127, 0, 0, 1].into())),
            Origin::Local
        ));
    }

    #[test]
    fn revoked_device_token_falls_back_to_unpaired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry =
            crate::pairing::DeviceRegistry::open(&dir.path().join("devices.db"))
                .expect("registry");
        let (device, token) = registry.pair("旧手机").expect("pair");
        assert!(registry.revoke(&device.device_id));

        assert!(
            registry.authenticate(&token).is_none(),
            "撤销后凭证立即失效"
        );
        assert!(matches!(
            derive_origin(None, Some([192, 168, 1, 50].into())),
            Origin::Unpaired
        ));
    }

    #[test]
    fn policy_guard_admin_header() {
        let mut p = PolicyGuardConfig::default();
        p.admin_key = Some(Arc::from("secret-token"));
        let mut ok = HeaderMap::new();
        ok.insert(
            axum::http::HeaderName::from_static("x-hermes-policy-admin"),
            HeaderValue::from_static("secret-token"),
        );
        assert!(p.check_mutation_admin(&ok).is_ok());

        let bad = HeaderMap::new();
        assert!(p.check_mutation_admin(&bad).is_err());
    }

    #[test]
    fn policy_guard_actor_allowlist() {
        let p = PolicyGuardConfig {
            admin_key: None,
            require_admin: false,
            allowed_actors: vec!["ops".to_string(), "trainer".to_string()],
            export_require_admin: false,
        };
        assert!(p.check_actor("ops").is_ok());
        assert!(p.check_actor("trainer").is_ok());
        assert!(p.check_actor("hacker").is_err());
    }

    /// §5.5 第 5 层：IP→Origin 推导。
    /// 当前简化版（设备配对流程未建）：loopback = Local，其他远程 = Unpaired。
    #[test]
    fn loopback_ip_resolves_to_local_origin() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(ip.is_loopback());
    }

    #[test]
    fn remote_ip_is_not_loopback() {
        for ip in [
            "10.0.0.1".parse::<IpAddr>().unwrap(),
            "192.168.1.100".parse::<IpAddr>().unwrap(),
        ] {
            assert!(!ip.is_loopback(), "expected non-loopback: {ip}");
        }
    }

    // ----- §5.5 第 4 层细粒度授权：设备能力位 ∩ 静态矩阵 -----

    #[test]
    fn parse_device_scopes_maps_names_to_capabilities() {
        let set = parse_device_scopes(r#"["chat","dangeroustool"]"#);
        assert!(set.contains(&Capability::Chat));
        assert!(set.contains(&Capability::DangerousTool));
        assert!(!set.contains(&Capability::ConfigMutation));
    }

    #[test]
    fn parse_device_scopes_empty_and_unknown_tokens_yield_empty_set() {
        // 占位 '[]' = 未配任何能力位，全拒绝（保守侧）。
        let set = parse_device_scopes("[]");
        assert!(set.is_empty());
        // 未知能力位名忽略（不授予，矩阵仍兜底）。
        let set = parse_device_scopes(r#"["superpower","chat"]"#);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&Capability::Chat));
    }

    /// 矩阵 ∩ 能力位语义：矩阵 `Denied` 的即使能力位误配也保持
    /// `Denied`（纵深防御）；矩阵 `Allowed`/`NeedsLocalConfirmation`
    /// 的，能力位未配即降为 `Denied`。
    #[test]
    fn effective_decision_intersects_matrix_and_scopes() {
        use crate::pairing::PairedDevice;
        let dev = |scopes: &str| PairedDevice {
            device_id: "dev-x".into(),
            device_name: "x".into(),
            scopes: scopes.into(),
            issued_at: 0,
            last_seen_at: None,
            revoked_at: None,
        };
        let origin = Origin::PairedDevice {
            device_id: "dev-x".into(),
        };

        // 最小集 ["chat"]：Chat 放行，DangerousTool 仍走矩阵的
        // NeedsLocalConfirmation（矩阵给"最多能做什么"，能力位给
        // "实际被授予什么"——DangerousTool 在最小集里未配，被降为 Denied）。
        let chat_only = dev(r#"["chat"]"#);
        assert_eq!(
            effective_decision_for_paired(&origin, Capability::Chat, Some(&chat_only)),
            Decision::Allowed,
        );
        assert_eq!(
            effective_decision_for_paired(&origin, Capability::DangerousTool, Some(&chat_only)),
            Decision::Denied,
            "DangerousTool 未在最小集能力位中 → 降为 Denied"
        );

        // 矩阵 Denied 的（ConfigMutation）即使能力位误配也保持 Denied。
        let mis_configured = dev(r#"["configmutation","chat"]"#);
        assert_eq!(
            effective_decision_for_paired(&origin, Capability::ConfigMutation, Some(&mis_configured)),
            Decision::Denied,
            "矩阵 Denied 的能力位误配不能提权"
        );

        // DangerousTool 在能力位中配了：矩阵给 NeedsLocalConfirmation，
        // 能力位放行后保持矩阵判定（不强行升 Allowed）。
        let with_dangerous = dev(r#"["chat","dangeroustool"]"#);
        assert_eq!(
            effective_decision_for_paired(&origin, Capability::DangerousTool, Some(&with_dangerous)),
            Decision::NeedsLocalConfirmation,
        );

        // 占位 '[]'：全拒绝（能力位未配任何项）。
        let empty = dev("[]");
        assert_eq!(
            effective_decision_for_paired(&origin, Capability::Chat, Some(&empty)),
            Decision::Denied,
        );

        // 非设备来源（Local / Unpaired）无能力位维度，透传矩阵判定。
        assert_eq!(
            effective_decision_for_paired(&Origin::Local, Capability::Chat, None),
            Decision::Allowed,
        );
        assert_eq!(
            effective_decision_for_paired(&Origin::Unpaired, Capability::Chat, None),
            Decision::Denied,
        );
    }

    /// A 半边验收：`pair()` 写入最小能力集（非恒占位 `'[]'`），
    /// `authenticate` 读回同一份（设备能力位的落库→读库闭环）。
    #[test]
    fn pair_writes_default_minimal_scopes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry =
            crate::pairing::DeviceRegistry::open(&dir.path().join("devices.db"))
                .expect("registry");
        let (device, token) = registry.pair("能力位手机").expect("pair");
        assert_eq!(
            device.scopes,
            crate::pairing::DEFAULT_DEVICE_SCOPES,
            "pair 应写最小能力集而非恒占位"
        );
        // 读库闭环：authenticate 读回同一份 scopes。
        let authed = registry.authenticate(&token).expect("token 有效");
        assert_eq!(authed.scopes, device.scopes);
        // 且解析后恰含 Chat 一项（最小集语义）。
        let set = parse_device_scopes(&authed.scopes);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&Capability::Chat));
    }

    // ----- §5.5 对策 T6：暴力破解 IP 锁定（AuthFailLockout）-----

    /// 窗口内连续 threshold 次失败 → 该 IP 进入锁定。
    #[tokio::test]
    async fn lockout_triggers_after_threshold_failures() {
        // threshold=3, 1s 窗口（测试内多次失败都落在窗口内）, 1s 冷却。
        let lo = AuthFailLockout::new(3, 1, 1);
        assert!(!lo.is_locked("1.1.1.1").await);
        lo.record_failure("1.1.1.1").await;
        lo.record_failure("1.1.1.1").await;
        assert!(!lo.is_locked("1.1.1.1").await, "未达阈值不应锁定");
        lo.record_failure("1.1.1.1").await;
        assert!(lo.is_locked("1.1.1.1").await, "达阈值应锁定");
    }

    /// 锁定按 IP 隔离：一个 IP 锁定不影响另一个 IP。
    #[tokio::test]
    async fn lockout_is_per_ip() {
        let lo = AuthFailLockout::new(2, 60, 60);
        lo.record_failure("a").await;
        lo.record_failure("a").await;
        assert!(lo.is_locked("a").await);
        assert!(!lo.is_locked("b").await, "IP 间不串扰");
    }

    /// 认证成功清零：输错 2 次后输对 → 计数清 0，不再锁定。
    #[tokio::test]
    async fn success_resets_failure_counter() {
        let lo = AuthFailLockout::new(3, 60, 60);
        lo.record_failure("1.1.1.1").await;
        lo.record_failure("1.1.1.1").await;
        lo.record_success("1.1.1.1").await;
        assert!(!lo.is_locked("1.1.1.1").await);
        // 清零后重新计数，需再 3 次才锁定。
        lo.record_failure("1.1.1.1").await;
        lo.record_failure("1.1.1.1").await;
        assert!(!lo.is_locked("1.1.1.1").await, "清零后未达新阈值");
    }

    /// 冷却期到期自动解锁：锁定后 sleep 过冷却 → is_locked 转 false。
    #[tokio::test]
    async fn lockout_expires_after_cooldown() {
        // 冷却 1s：锁定 1s 后自动解锁。
        let lo = AuthFailLockout::new(1, 60, 1);
        lo.record_failure("2.2.2.2").await; // threshold=1 → 立即锁定
        assert!(lo.is_locked("2.2.2.2").await);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!lo.is_locked("2.2.2.2").await, "冷却到期应自动解锁");
    }

    /// 持续失败续期冷却：锁定后攻击继续 → 冷却不断延长（防"刚好过
    /// 冷却就重来"的绕锁）。
    #[tokio::test]
    async fn ongoing_failures_extend_cooldown() {
        let lo = AuthFailLockout::new(1, 60, 1);
        lo.record_failure("3.3.3.3").await; // 锁定 1s
        assert!(lo.is_locked("3.3.3.3").await);
        // 冷却期内又来一次失败 → 续期（锁定延长，不会在 1.1s 时解锁）。
        lo.record_failure("3.3.3.3").await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            lo.is_locked("3.3.3.3").await,
            "持续失败应续期冷却，不会准时解锁"
        );
    }
}
