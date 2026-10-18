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
use axum::http::{Request, Response, StatusCode};
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
    /// 是否信任 `X-Forwarded-*` 等代理证据头（`HERMIX_TRUST_FORWARDED=1`）。
    ///
    /// 默认**不信任**：loopback 来源一旦带了转发证据，就不再判 `Local`。
    /// 理由是同机反代（nginx / vite proxy）会把外部请求转成 `127.0.0.1`，
    /// 只看对端 IP 等于把「公网请求」认证成「本机请求」。
    pub trust_forwarded: bool,
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
            .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);
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
            trust_forwarded: env_compat("HERMIX_TRUST_FORWARDED", "HERMES_TRUST_FORWARDED")
                .as_deref()
                == Some("1"),
        }
    }
}

/// 请求是否带「经过了代理」的证据头。
///
/// `X-Forwarded-For` / `Forwarded` / `X-Real-IP` / `X-Forwarded-Host` 任一出现
/// 即算。同机反代会把对端重写成 `127.0.0.1`，此时对端地址不再证明「请求来自
/// 本机进程」——只有一个本机进程才会不带这些头直接连 loopback。
fn has_forwarded_evidence(headers: &axum::http::HeaderMap) -> bool {
    ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip"]
        .iter()
        .any(|h| headers.contains_key(*h))
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

/// 每分钟每来源的默认请求上限。
///
/// 600 的依据是 dashboard 的轮询频率（2s 一次 ≈ 30/min）留 20 倍余量，
/// 同时让「6 位配对码在线枚举」在 60 分钟 TTL 内最多只能试 3.6 万次，
/// 配合配对码自身的 5 次预算，枚举不再可行。
///
/// 旧默认值是 `0`，而 `0` 在 [`RateLimiter::allow`] 里等于**完全不限流**，
/// 于是「有全局限流」只存在于注释里。现在要关掉必须显式设置
/// `HERMIX_HTTP_RATE_LIMIT_PER_MINUTE=0`。
pub const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 600;

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            per_minute,
        }
    }

    /// `per_minute == 0` = 显式关闭（只可能来自 env 的显式配置）。
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

/// Compare two secrets without leaking their common prefix length.
///
/// `str::eq` short-circuits on the first differing byte, so its wall
/// clock correlates with how much of the `api_key` a guesser got right.
/// Hand-rolled rather than pulling in `subtle` — the six-step ladder
/// says standard library before new dependencies, and this is five
/// lines. The length branch is unavoidable (it leaks length, not
/// content) but api_key length is not a secret.
fn ct_eq(a: Option<&str>, b: Option<&str>) -> bool {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a.as_bytes(), b.as_bytes()),
        _ => return false,
    };
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
///
/// `pub(crate)` 是为了让**非 HTTP 面**也能用同一份来源判定——目前只有
/// `/v1/ws-stream` 的 upgrade handler（手机端经 relay 接入）。它自己重写一套
/// 就会绕过本模块的未配对闸门、IP allowlist 与能力位交集。
pub(crate) fn derive_origin(
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
/// 端点级能力位判定（§5.5 第 5 层 HTTP 端点消费点）。直接吃
/// `request_guard` 装进 Extension 的 `HashSet<Capability>`（设备能力
/// 位集），而非 `PairedDevice` 字符串——省掉一次 `parse_device_scopes`。
/// 语义同上：矩阵 `Denied` 保持
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

/// 设备鉴权的**非 HTTP 面**入口：给 WS upgrade handler 用。
///
/// 存在的理由是「同一份判定只能有一处」。`request_guard` 是 HTTP 面的实现；
/// 手机端经 relay 从公网打 `/v1/ws-stream`，走不到 `request_guard`，若在
/// handler 里自己解析 Bearer、自己验签名，就等于有第二套判定——绕过未配对
/// 闸门、能力位交集与失败预算是迟早的事。
///
/// 判定链与 HTTP 面完全一致：
/// 1. `Authorization: Bearer <token>` → `DeviceRegistry::authenticate`；
/// 2. **已配对设备必须另附请求签名**（T4 对策，见 `request_signature.rs:6-10`）——
///    `HMAC_SHA256(token, METHOD\npath\nt\nnonce)`，时间窗 + nonce 单次使用，
///    所以「token 泄漏 ≠ 无限使用」；
/// 3. 记 `AuditEvent::Auth`（命中 allowed / 出示但未命中 denied）。
///
/// 返回 `Ok(device)` 时调用方应继续用 [`derive_origin`] + 能力位交集做
/// 授权；本函数只回答「你是谁」。
pub(crate) fn authenticate_device_request(
    headers: &axum::http::HeaderMap,
    method: &str,
    path: &str,
    devices: Option<&std::sync::Arc<crate::pairing::DeviceRegistry>>,
    nonce_cache: &crate::request_signature::NonceCache,
) -> Result<crate::pairing::PairedDevice, DeviceAuthError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(str::trim))
        .filter(|s| !s.is_empty());
    let Some(token) = token else {
        return Err(DeviceAuthError::NoToken);
    };
    let device = devices
        .and_then(|r| r.authenticate(token))
        .ok_or(DeviceAuthError::InvalidToken)?;
    // 签名与 token 一一对应：没有有效签名就当没配对处理，而不是放行。
    let sig_ok = crate::request_signature::signature_header(headers)
        .and_then(|h| {
            crate::request_signature::verify_signature(
                h,
                token,
                method,
                path,
                chrono::Utc::now().timestamp(),
                nonce_cache,
            )
            .ok()
        })
        .is_some();
    if !sig_ok {
        return Err(DeviceAuthError::BadSignature);
    }
    if let Some(reg) = devices {
        reg.record_audit(
            crate::pairing::AuditEvent::Auth,
            Some(device.device_id.clone()),
            None,
            Some("allowed".to_string()),
        );
    }
    Ok(device)
}

/// 设备鉴权失败原因。**故意不细分 token 格式错误与签名错误给客户端看**——
/// 攻击者据此区分「token 存在但签名错」与「token 不存在」，是免费的 oracle。
/// 对外统一 401，日志里才记具体原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceAuthError {
    /// 没有 `Authorization: Bearer`（或为空）。
    NoToken,
    /// token 无效 / 已撤销。
    InvalidToken,
    /// 配对设备缺有效请求签名。
    BadSignature,
}

impl DeviceAuthError {
    /// 对外响应体。统一 401 + 不含原因的短句。
    pub(crate) fn as_response(self) -> axum::response::Response {
        use axum::response::IntoResponse;
        let detail = match self {
            Self::NoToken | Self::InvalidToken => "missing or invalid device token",
            Self::BadSignature => "missing or invalid request signature",
        };
        tracing::debug!(error = ?self, "device auth rejected on non-HTTP face");
        (axum::http::StatusCode::UNAUTHORIZED, detail).into_response()
    }
}

pub async fn request_guard(
    security: Arc<HttpSecurity>,
    rate: Arc<RateLimiter>,
    devices: Option<Arc<crate::pairing::DeviceRegistry>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    // CORS preflight carries no credentials by definition, so it cannot
    // be authenticated — but the unconditional early return that used
    // to sit here also skipped `ip_allowed`, the auth-fail lockout, and
    // the rate limiter, making OPTIONS a free unauthenticated flood
    // vector. Preflight is now rate-limited like any other request;
    // the CORS layer itself (`http_cors_layer`) still answers it.
    if req.method() == axum::http::Method::OPTIONS {
        if !ip_allowed(&security, &req) {
            hermix_telemetry::record_http_reject();
            return (StatusCode::FORBIDDEN, "client IP not in allowlist").into_response();
        }
        let ck = client_key(&req);
        if !rate.allow(ck).await {
            hermix_telemetry::record_http_reject();
            return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
        }
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

    // Bearer 凭证只走 Authorization 头。
    //
    // 历史上有第二条通道：`?token=` query 传参。它让配对设备 token 和
    // 网关 api_key 出现在 URL 里 —— 进 TraceLayer 日志、浏览器历史、
    // 代理 Referer。仓库内没有任何客户端在用这条通道（客户端的
    // Authorization 头全部走裸 `fetch`），所以是纯粹的攻击面而非
    // 兼容性负担，直接移除。
    //
    // 指纹：`crates/hermix-server/src/lib.rs::redact_query_credentials`
    // 仍保留，它防御的是任意第三方在 URL 里塞 token 的情况，与本处
    // 是否接受无关。
    let token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(str::trim))
        .map(std::string::ToString::to_string);
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
        let api_key_ok = ct_eq(token.as_deref(), Some(key.as_ref()));
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
        if device.is_some() || ct_eq(token.as_deref(), security.api_key.as_deref()) {
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
    //
    // loopback 只证明「对端是本机」，不证明「请求由本机进程发起」：同机反代
    // （nginx / vite proxy）把外部请求转成 127.0.0.1 时会照抄客户端的转发头。
    // 出现转发证据就不再把来源判成 `Local`（最宽权限），降为 `Unpaired`
    // ——要么带上设备凭据，要么去走配对端点。确需信任反代时用
    // `HERMIX_TRUST_FORWARDED=1` 显式恢复旧行为。
    let mut origin = derive_origin(device.as_ref(), client_ip(&req));
    if matches!(origin, Origin::Local)
        && !security.trust_forwarded
        && has_forwarded_evidence(req.headers())
    {
        tracing::warn!(
            path = %path,
            "loopback 请求带转发头 —— 不按本机来源处理（HERMIX_TRUST_FORWARDED=1 可显式信任）"
        );
        origin = Origin::Unpaired;
    }
    // 已鉴权的客户端刚说过话 —— 记一笔「有人在」。
    // 审批超时按可送达时间计费就靠这个信号：人在、或是有人能看见审批，才计时。
    if !matches!(origin, Origin::Unpaired) {
        crate::presence::touch();
    }

    // 未配对来源一律 403，**没有任何路径豁免**。
    //
    // 这里原本放行 `/api/pairing/complete` 与 `/pair`——两条路由在配对改走 relay
    // （2026-10-10）之后就不存在了。豁免留着是地雷：将来谁在这个名字上新建路由，
    // 它会**默认不带认证**。配对现在走 relay 帧（`pairing::handle_pairing_frame`），
    // 不经过这个 HTTP 面。
    //
    // `/health` 与 `/metrics` 不靠这条分支——它们在更早的地方就返回了。
    if matches!(origin, Origin::Unpaired) {
        hermix_telemetry::record_http_reject();
        // 未配对闸门是**认证失败**的一种：不计入失败预算的话，攻击者可以
        // 拿无限次 403 免费探测路径存在性，且这条分支永远不触发锁定。
        if let Some(lockout) = &security.auth_fail_lockout {
            lockout.record_failure(&client_key(&req)).await;
        }
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

    /// 设备鉴权辅助（WS 等非 HTTP 面用）。四种路径都要覆盖：这条是手机端
    /// 唯一能进 `/v1/ws-stream` 的门。
    #[test]
    fn 非http面设备鉴权四路径() {
        use axum::http::header::AUTHORIZATION;
        use axum::http::{HeaderMap, HeaderValue};
        use std::sync::Arc;

        fn setup() -> (tempfile::TempDir, Arc<crate::pairing::DeviceRegistry>, String) {
            let dir = tempfile::tempdir().expect("tempdir");
            let reg = Arc::new(
                crate::pairing::DeviceRegistry::open(&dir.path().join("devices.db"))
                    .expect("registry"),
            );
            let (_d, token) = reg.pair("测试手机").expect("pair");
            (dir, reg, token)
        }

        fn bearer(token: &str) -> HeaderMap {
            let mut h = HeaderMap::new();
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).expect("hdr"),
            );
            h
        }

        fn signed(token: &str, method: &str, path: &str, nonce: &str) -> HeaderMap {
            let t = chrono::Utc::now().timestamp();
            let sig = crate::request_signature::compute_signature(token, method, path, t, nonce);
            let mut h = bearer(token);
            h.insert(
                "x-hermix-signature",
                HeaderValue::from_str(&format!("t={t},n={nonce},s={sig}")).expect("hdr"),
            );
            h
        }

        const PATH: &str = "/v1/ws-stream/s1";

        // 正路：Bearer + 有效签名 → 拿到设备
        {
            let (_d, reg, token) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = signed(&token, "GET", PATH, "n1");
            let dev = authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache)
                .expect("应通过");
            assert_eq!(dev.device_id, reg.authenticate(&token).unwrap().device_id);
        }

        // 无 Authorization
        {
            let (_d, reg, _t) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = HeaderMap::new();
            assert_eq!(
                authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache).unwrap_err(),
                DeviceAuthError::NoToken
            );
        }

        // token 无效
        {
            let (_d, reg, _t) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = bearer("hmxd_bogus");
            assert_eq!(
                authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache).unwrap_err(),
                DeviceAuthError::InvalidToken
            );
        }

        // token 对但**没签名** —— 这是手机端最容易漏的一条：Bearer 本身
        // 不构成授权（§5.5 T4）。
        {
            let (_d, reg, token) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = bearer(&token);
            assert_eq!(
                authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache).unwrap_err(),
                DeviceAuthError::BadSignature
            );
        }

        // nonce 重放：同一份签名头再发一次必须被拒（token 泄漏≠无限使用）
        {
            let (_d, reg, token) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = signed(&token, "GET", PATH, "replay-me");
            assert!(authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache).is_ok());
            assert_eq!(
                authenticate_device_request(&h, "GET", PATH, Some(&reg), &cache).unwrap_err(),
                DeviceAuthError::BadSignature,
                "重放必须失败"
            );
        }

        // 签名覆盖 method + path：换一个 path 就不该通过（relay 一旦重写
        // 路径，手机端的签名就全废——这是 relay 必须字节级透传的根因）。
        {
            let (_d, reg, token) = setup();
            let cache = crate::request_signature::NonceCache::new();
            let h = signed(&token, "GET", PATH, "n2");
            assert_eq!(
                authenticate_device_request(&h, "GET", "/v1/other", Some(&reg), &cache).unwrap_err(),
                DeviceAuthError::BadSignature
            );
        }
    }

    /// 未配对关口对**任何**路径都不豁免。
    ///
    /// 曾经放行 `/api/pairing/complete` 与 `/pair`（配对改走 relay 前的旧端点），
    /// 那两条路由已不存在；豁免留着等于给同名的未来路由预开一个免认证口子。
    #[test]
    fn unpaired_gate_has_no_exemptions() {
        // 远程无凭证 = Unpaired。
        assert!(matches!(
            derive_origin(None, Some([192, 168, 1, 50].into())),
            Origin::Unpaired
        ));
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
        let origin = Origin::PairedDevice {
            device_id: "dev-x".into(),
        };

        // 最小集 ["chat"]：Chat 放行，DangerousTool 仍走矩阵的
        // NeedsLocalConfirmation（矩阵给"最多能做什么"，能力位给
        // "实际被授予什么"——DangerousTool 在最小集里未配，被降为 Denied）。
        let chat_only = parse_device_scopes(r#"["chat"]"#);
        assert_eq!(
            effective_decision_with_scopes(&origin, Capability::Chat, Some(&chat_only)),
            Decision::Allowed,
        );
        assert_eq!(
            effective_decision_with_scopes(&origin, Capability::DangerousTool, Some(&chat_only)),
            Decision::Denied,
            "DangerousTool 未在最小集能力位中 → 降为 Denied"
        );

        // 矩阵 Denied 的（ConfigMutation）即使能力位误配也保持 Denied。
        let mis_configured = parse_device_scopes(r#"["configmutation","chat"]"#);
        assert_eq!(
            effective_decision_with_scopes(&origin, Capability::ConfigMutation, Some(&mis_configured)),
            Decision::Denied,
            "矩阵 Denied 的能力位误配不能提权"
        );

        // DangerousTool 在能力位中配了：矩阵给 NeedsLocalConfirmation，
        // 能力位放行后保持矩阵判定（不强行升 Allowed）。
        let with_dangerous = parse_device_scopes(r#"["chat","dangeroustool"]"#);
        assert_eq!(
            effective_decision_with_scopes(&origin, Capability::DangerousTool, Some(&with_dangerous)),
            Decision::NeedsLocalConfirmation,
        );

        // 占位 '[]'：全拒绝（能力位未配任何项）。
        let empty = parse_device_scopes("[]");
        assert_eq!(
            effective_decision_with_scopes(&origin, Capability::Chat, Some(&empty)),
            Decision::Denied,
        );

        // 非设备来源（Local / Unpaired）无能力位维度，透传矩阵判定。
        assert_eq!(
            effective_decision_with_scopes(&Origin::Local, Capability::Chat, None),
            Decision::Allowed,
        );
        assert_eq!(
            effective_decision_with_scopes(&Origin::Unpaired, Capability::Chat, None),
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

    // ----- 代理证据头：loopback 不再天然等于「本机来源」 -----

    /// 同机反代会把外部请求转成 `127.0.0.1`。出现转发证据时，来源必须
    /// 降级（否则「公网请求」被认证成「本机请求」，拿到 Local 的全宽权限）。
    #[test]
    fn forwarded_headers_are_recognised() {
        for h in ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip"] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::HeaderName::from_static(h),
                axum::http::HeaderValue::from_static("203.0.113.9"),
            );
            assert!(has_forwarded_evidence(&headers), "{h} 应被识别为转发证据");
        }
        assert!(
            !has_forwarded_evidence(&axum::http::HeaderMap::new()),
            "没有转发头时不应误判"
        );
    }

    /// loopback 没有转发证据时仍然是 `Local`（桌面 stdio 桥就是这样进来的）。
    #[test]
    fn loopback_without_forwarding_stays_local() {
        assert_eq!(
            derive_origin(None, Some([127, 0, 0, 1].into())),
            Origin::Local
        );
        assert!(!has_forwarded_evidence(&axum::http::HeaderMap::new()));
    }

    // ----- CORS：默认只回显本机来源 -----

    #[test]
    fn local_origins_are_recognised() {
        for o in [
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://asset.localhost",
            "http://localhost:1420",
            "http://127.0.0.1:3000",
            "https://127.0.0.1",
            "http://[::1]:8443",
            "tauri://localhost",
        ] {
            assert!(crate::is_local_origin(o), "{o} 应被认作本机来源");
        }
    }

    /// 这是本次收紧的核心断言：第三方站点不能再靠 `Access-Control-Allow-Origin`
    /// 回显读到本机 API 的响应体。
    #[test]
    fn foreign_origins_are_rejected() {
        for o in [
            "https://evil.example.com",
            "https://localhost.evil.com",
            "https://127.0.0.1.evil.com",
            "http://tauri.localhost.evil.com",
            "null",
        ] {
            assert!(!crate::is_local_origin(o), "{o} 不应被认作本机来源");
        }
    }

    // ----- 限流默认值不再等于「不限流」 -----

    #[test]
    fn default_rate_limit_is_finite() {
        assert!(
            DEFAULT_RATE_LIMIT_PER_MINUTE > 0,
            "默认值必须有限：0 在 allow() 里等于完全不限流"
        );
    }

    #[tokio::test]
    async fn zero_still_means_explicitly_disabled() {
        let rl = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(rl.allow("k".to_string()).await, "显式 0 仍是关闭限流");
        }
    }

    #[tokio::test]
    async fn limiter_blocks_after_quota() {
        let rl = RateLimiter::new(2);
        assert!(rl.allow("k".to_string()).await);
        assert!(rl.allow("k".to_string()).await);
        assert!(!rl.allow("k".to_string()).await, "第 3 次应被拒");
    }
}
