#![allow(clippy::doc_lazy_continuation, clippy::field_reassign_with_default)]
//! HTTP and WebSocket API server for Hermes.
//!
//! Environment (see also `security` module):
//! - `HERMES_HTTP_MAX_BODY_BYTES` — max JSON body size for POST routes (default 2 MiB).
//! - `HERMES_HTTP_CORS_ORIGINS` — comma-separated browser Origins for CORS; empty → permissive.
//! - `HERMES_SERVE_WEB_STATIC` — set `0`/`false`/`off` to skip serving `apps/dashboard/dist` (API-only).
//! Policy HTTP routes are intentionally omitted (Hermes Python does not expose them).

pub mod dashboard;
pub mod test_env;
mod automation_trigger;
mod orchestration_bus;
mod task_tracker_adapter;
mod task_record_sink;
mod security;
pub mod crash_dump;
mod mobile;
mod mobile_protocol;
mod mobile_ws;
mod relay_client;
mod relay_crypto;
mod relay_identity;
mod relay_proto;
mod relay_settings;
mod relay_transport;
mod ws_transport;
mod metrics;
mod filesearch;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_state;

pub use security::parse_allowed_ips;

//Re-export orchestration event bus types
pub use orchestration_bus::{
    OrchestrationEvent, OrchestrationEventBus, SharedOrchestrationBus,
};

use std::convert::Infallible;
use hermix_config::env_compat;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use base64::Engine as _;

use axum::body::Body;
use axum::http::header;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::Utc;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use hermix_config::GatewayConfig;
use hermix_agent::python_alignment::ThinkBlockParser;
use hermix_agent::ArchiveRef;
use hermix_core::{
    traits::{PermissionLevel, RiskLevel},
    AgentError, Message, MessageRole, StreamChunk,
};
use hermix_tools::approval::UserAllowlist;
use regex::Regex;
use hermix_transport::{
    CreateSessionRequest, ListSessionsResponse, ProtocolMessage, RenameSessionRequest,
    SendMessageRequest, SendMessageResponse, SessionMessagesResponse, SessionSummary, StreamEvent,
    WsClientMessage, WsEnvelope,
};

//T2-A : Session search types live in `dashboard::types` (they
// are dashboard-shaped and not part of the core transport). Will move them
// to `hermix-transport` if the SPA ever needs them on /v1/.
use crate::dashboard::types::{SessionSearchResponse, SessionSearchResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tower::service_fn;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

pub const HTTP_PLATFORM: &str = "http";

/// Default timeout for pending tool-call approval requests.
/// Mirrors the upstream `approvals.timeout: 60s` in the Hermes
/// 7-layer defense model: if the WS client doesn't send an
/// `ApprovalResponse` within this window, the request is
/// auto-resolved to `false` (deny) and removed from the
/// registry. This prevents a hung UI from holding the agent
/// loop indefinitely, and it shifts the burden of stale
/// requests to deny-by-default (fail-closed) -- which is the
/// only safe direction for a destructive-tool approval.
/// **bump:** raised from 60s → 300s. The 60s
/// ceiling produced a user-facing race: a user who paused to
/// read the risk level, then clicked "允许" at second 61, saw
/// the card flip to "已允许" client-side, but the backend
/// had already auto-denied via this timeout, so the LLM
/// received `false` and proceeded as denied. The user-visible
/// result was "I clicked allow but the agent said denied" —
/// the exact symptom the owner reported. 5 minutes is long
/// enough to read a command and make a decision, short
/// enough that a stuck UI still resolves. The frontend
/// shows a countdown so the user always knows the budget.
/// lowered 300s → 30s after type-B recurrence
/// showed that 5 minutes of "● ● ● 正在思考" felt permanent to
/// Zero means **no auto-deny timeout** — the approval card stays
/// open indefinitely until the user explicitly clicks 允许 or 拒绝
/// (`ApprovalConfig.timeout_secs = 0` 可恢复旧行为). 默认 600s：
/// 旧行为（ZERO，无限等待）在"人不在电脑前"场景下等于永久挂死——
/// agent 停在 AwaitApproval，没有队列、没有恢复路径。改成有界等待
/// 后：到期自动 deny（超时事实写入时间轴），配合远程审批（IM/移动端）
/// 覆盖"人不在但想放行"的场景。运行时实际生效值以
/// `hermix_config::approval_timeout_secs()` 为准（启动时从
/// `ApprovalConfig.timeout_secs` 写入），本常量只是缺省值。
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// Compute the wall-clock deadline (Unix milliseconds) for a fresh
/// approval request. Centralized so the WS envelope and the persisted
/// `ApprovalRecord` share the **same** value — reloading the session
/// would otherwise show a countdown that doesn't match the original
/// (the two `now()` calls taken a few microseconds apart diverge
/// visibly when the deadline is rounded to seconds for display).
/// Returns `None` when `DEFAULT_APPROVAL_TIMEOUT` is zero — no
/// deadline means the front-end countdown pill is hidden and the
/// card stays open until the user explicitly resolves it.
/// Otherwise returns `Some(now + timeout_ms)` so the front-end can
/// render a live countdown. Monotonic clock is not appropriate
/// here — we want wall-clock so the deadline survives a clock-jump
/// and matches the user's `Date.now()` on the front-end (which is
/// also wall-clock).
pub fn compute_approval_deadline_ms() -> Option<i64> {
let secs = hermix_config::approval_timeout_secs();
if secs == 0 {
return None;
}
Some(chrono::Utc::now().timestamp_millis() + (secs as i64) * 1000)
}

// ---------------------------------------------------------------------------
// ApprovalRegistry — maps session → request_id → oneshot::Sender<bool>
// ---------------------------------------------------------------------------

/// Thread-safe registry for pending tool-call approval requests.
/// When the agent needs user approval before executing a risky tool, it
/// creates a `oneshot::Sender<bool>` keyed by `(session_id, request_id)`.
/// The client resolves the request via a [`WsClientMessage::ApprovalResponse`]
/// message on the same WebSocket, which looks up and completes the channel.

pub mod approval;
pub mod attachments;
pub mod channels;
pub mod coalescer;
pub mod pairing;
pub mod request_signature;
pub mod tls;
pub use approval::ApprovalRegistry;

pub mod state;
pub use state::{ClarifyBroadcast, HttpServerState};
pub mod errors;
pub use errors::HttpError;
pub mod voice;
pub mod transport;
pub mod stdio_session;
pub mod ws;
pub mod ws_grace;
pub mod util;
pub(crate) use util::*;
pub mod routes;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandResponse {
    pub accepted: bool,
    pub output: String,
}

fn max_request_body_bytes() -> usize {
    const DEFAULT: usize = 2 * 1024 * 1024;
    env_compat("HERMIX_HTTP_MAX_BODY_BYTES", "HERMES_HTTP_MAX_BODY_BYTES")
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
}

/// Strip credential-looking query parameters from a URI before it is
/// written to the request trace span.
///
/// The server no longer accepts a credential from the query string
/// (`?token=` was removed — see `security::request_guard`), so this is
/// defense in depth against a client or third-party page putting a secret
/// in a URL. Redacts the value but keeps the key and the rest of the
/// query, so the trace still shows *that* something was supplied.
fn redact_query_credentials(uri: &axum::http::Uri) -> String {
    const SECRET_KEYS: [&str; 3] = ["token", "api_key", "apikey"];
    let Some(query) = uri.query() else {
        return uri.to_string();
    };
    let redacted: Vec<String> = query
        .split('&')
        .map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or_default();
            if parts.next().is_some_and(|_| {
                SECRET_KEYS
                    .iter()
                    .any(|s| key.eq_ignore_ascii_case(s))
            }) {
                format!("{}=<redacted>", key)
            } else {
                pair.to_string()
            }
        })
        .collect();
    format!("{}?{}", uri.path(), redacted.join("&"))
}

/// CORS: comma-separated allowlist in `HERMES_HTTP_CORS_ORIGINS` (e.g. `https://app.example.com`).
/// Empty / invalid → permissive (local dev / same-origin split defaults).
fn http_cors_layer() -> CorsLayer {
    let raw = env_compat("HERMIX_HTTP_CORS_ORIGINS", "HERMES_HTTP_CORS_ORIGINS").unwrap_or_default();
    let origins: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if origins.is_empty() {
        tracing::debug!("HERMES_HTTP_CORS_ORIGINS unset — using permissive CORS");
        return CorsLayer::permissive();
    }
    let mut header_values: Vec<HeaderValue> = Vec::new();
    for o in origins {
        match o.parse::<HeaderValue>() {
            Ok(v) => header_values.push(v),
            Err(_) => tracing::warn!(origin = %o, "invalid CORS origin, skipped"),
        }
    }
    if header_values.is_empty() {
        tracing::warn!("HERMES_HTTP_CORS_ORIGINS had no valid origins — permissive CORS");
        return CorsLayer::permissive();
    }
    tracing::info!(count = header_values.len(), "CORS allowlist enabled");
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(header_values))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(AllowHeaders::list([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
        ]))
}

pub fn router(state: HttpServerState) -> Router {
    let security = Arc::new(security::HttpSecurity::from_env());
    let rate = Arc::new(security::RateLimiter::new(security.rate_limit_per_minute));
    let sec_guard = security.clone();
    let rate_guard = rate.clone();
    let device_registry = state.device_registry.clone();
    let body_limit = max_request_body_bytes();

    let plugin_dir = state.hermix_home.join("dashboard-plugins");
    if let Err(e) = std::fs::create_dir_all(&plugin_dir) {
        tracing::warn!(
            error = %e,
            dir = %plugin_dir.display(),
            "could not create dashboard-plugins directory"
        );
    }

    let mut app = Router::new()
        .route("/health", get(health))
        .route("/shutdown", get(shutdown_server))
        // 本地文件搜索：提权 helper（hermix-filesearch.exe）的 HTTP 面。
        // helper 未运行时首个请求会触发 UAC（弹一次）。
        .route("/v1/filesearch/search", get(filesearch::search))
        .route("/v1/filesearch/status", get(filesearch::status))
        .route("/v1/filesearch/shutdown", post(filesearch::shutdown))
        // 浏览器小窗暂停/恢复：桌面端小窗隐藏时把「停 Chrome 侧 screencast 编码」
        // 的指令发到 sidecar（浏览器会话在那），desktop 自己进程里的 pause 是空操作。
        .route("/api/browser-view/paused", post(browser_view_paused))
        // 手机端（Hermix M）经自建 relay 从公网接入的流式面。
        //
        // **这个端点曾经被删**（`181a51aa`，2026-09-26 端点审计）：当时的
        // 理由是「桌面端全量迁 stdio bridge、前端零裸 `new WebSocket`、本项目
        // 没有移动端 crate」。桌面端那半确实迁完了，但「没有移动端」这个前提
        // 现在不成立了——手机端跨网络，stdio 管道够不着。看到它复活**不要当成
        // 回退删掉**：流式协议本体（`ws::handle_ws_stream`）从未改动，本次只补
        // upgrade handler 与 over-WebSocket 的 `Transport` 包装。
        //
        // 鉴权走 `security::authenticate_device_request`：Bearer + **请求签名**
        // 双要素，与 HTTP 面同一份判定。
        .route(
            "/v1/ws-stream/{session_id}",
            get(mobile_ws::ws_stream_upgrade),
        )
        // 手机端控制面。中断与任务列表都只按「本设备可见的 sid」返回，
        // 否则 A 手机能列出 B 手机的会话名与首行。
        .route(
            "/v1/sessions/{session_id}/interrupt",
            post(mobile::interrupt_session),
        )
        .route("/v1/tasks", get(mobile::list_tasks))
        // Vault（凭据保险库）手机面：仅配对设备；E2EE 由 relay_transport
        // 层透明完成。列表永不含密码；reveal 按名称取单条（用户在手机
        // 对话里要凭据的场景，决策笔记 2026-10-09-vault-credentials）。
        .route("/v1/vault/entries", get(mobile::vault_entries))
        .route("/v1/vault/reveal/{name}", post(mobile::vault_reveal))
        // §5.5 第 3/4 层：设备配对。出码仅 Local（handler 内强制）；
        // 配对本身走 relay 密封帧（pairing.rs::handle_pairing_frame）。
        .route(
            "/api/pairing/sessions",
            post(pairing::create_pairing_session),
        )
        .route("/api/pairing/devices", get(pairing::list_devices))
        .route(
            "/api/pairing/devices/{device_id}",
            delete(pairing::revoke_device),
        )
        // §5.5 P6：审计日志读端点（DiagnosticsTab 数据面）。仅 Local，
        // 与 revoke_device 同一 DeviceManagement 鉴权面。
        .route("/api/pairing/audit", get(pairing::list_audit))
        // relay 出站连接的配置面。仅 Local：它读写的是这台电脑在公网中转上的
        // 接入身份与凭据，不是公开信息。同一路径分两次 route 注册——axum 会把
        // 两个 MethodRouter 合并，写成 `get(..).put(..)` 反而推不出 state 类型。
        .route("/api/relay/config", get(relay_settings::get_config))
        .route("/api/relay/config", put(relay_settings::put_config))
        // 向 relay 换一条手机凭据。桌面端代发是因为 node 凭据明文只在这侧。
        .route(
            "/api/relay/phone-credential",
            post(relay_settings::issue_phone),
        )
        // 重置本机 E2EE 身份与 relay 凭据（DPAPI 主密钥丢失后的显式恢复动作）。
        .route(
            "/api/relay/reset-identity",
            post(relay_settings::reset_identity),
        )
        // IM 通道（QQ / 企业微信官方机器人）。全部仅 Local：铸码、扫码绑定
        // 都是本机操作，凭据是能直接操控本机的机器人密钥。
        .route("/api/channels/pairing-code", post(channels::create_pairing_code))
        .route("/api/channels/status", get(channels::channel_status))
        .route("/api/channels/bind/status", get(channels::bind_status))
        .route("/api/channels/bind/start", post(channels::bind_start))
        .route("/api/channels/bind/poll", get(channels::bind_poll))
        .route("/api/channels/bind/cancel", post(channels::bind_cancel))
        .route("/api/channels/bind/unbind", post(channels::bind_unbind))
        .route("/v1/sessions", get(routes::list_sessions).post(routes::create_session))
        .route(
            "/v1/sessions/{session_id}",
            post(routes::rename_session).patch(routes::patch_session).delete(routes::delete_session),
        )
        .route(
            "/v1/sessions/{session_id}/messages",
            get(routes::get_session_messages).post(routes::send_message),
        )
        // Persist the frontend-built `unified_steps` timeline for the
        // most recent assistant turn (sent by the desktop client right
        // after a stream completes). Keeps inline tool interleaving
        // across session reloads.
        .route(
            "/v1/sessions/{session_id}/messages/unified_steps",
            post(routes::save_session_unified_steps),
        )
        // archive refs surface for the chat UI.
        // `GET /v1/sessions/{id}/archive-refs` returns the per-session
        // JSONL index written by `archive_context_section`. Empty
        // Vec when the session has never been archived.
        .route(
            "/v1/sessions/{session_id}/archive-refs",
            get(routes::get_session_archive_refs),
        )
        //T2-A : moved from /api/sessions/* (dashboard router).
        // The frontend now uses /v1/sessions/{id}/undo and .../restore and .../search.
        .route(
            "/v1/sessions/{session_id}/undo",
            post(routes::undo_session),
        )
        .route(
            "/v1/sessions/{session_id}/restore",
            post(routes::restore_session),
        )
        .route("/v1/sessions/search", get(routes::search_sessions))
        .route("/v1/sessions/{session_id}/lineage", get(routes::get_session_lineage))
        // P4 L3 订阅快照端点（设计 §3.5）：客户端重连 / 冷启动凭
        // since_seq 取回会话状态（本批统一回全量快照 + 进程 seq 锚）。
        .route(
            "/v1/sessions/{session_id}/subscribe",
            get(routes::subscribe_session_snapshot),
        )
        // Approval response REST fallback — when WebSocket delivery
        // fails (Tauri WebView2 can lose buffered messages on connection
        // reset), the frontend sends the approval via HTTP as a backup.
        .route(
            "/v1/sessions/{session_id}/approval",
            post(routes::approve_via_rest),
        )
        // Re-project approvals the client missed while its socket was
        // closed (task_in_flight settle / reconnect).
        .route(
            "/v1/sessions/{session_id}/approvals/pending",
            get(routes::list_pending_approvals),
        )
        // todo (task list) HTTP endpoints. The LLM
        // goes through the `todo` tool over WS; the desktop TodoPanel
        // hits these endpoints for hydration (GET) and manual
        // checkbox toggles (PATCH). See `list_todos` / `patch_todo`.
        .route("/v1/todos", get(routes::list_todos))
        .route(
            "/v1/todos/{todo_id}",
            patch(routes::patch_todo).delete(routes::delete_todo),
        )
        .route("/v1/commands", post(routes::exec_command))
        .route("/v1/llm/warmup", post(routes::warm_up_provider_handler))
        // Expert catalog (driven by the desktop client's bundled built-ins).
        .route("/v1/experts", get(routes::list_experts).post(routes::register_expert))
        .route("/v1/experts/{id}", get(routes::get_expert))
        // TP1: dynamic tool discovery — the desktop client
        // calls this once at startup and uses the result as the single
        // source of truth for `overrides.tools`. Adding a new tool to
        // the `ToolRegistry` automatically surfaces it in the chat UI
        // and the LLM's tool list, with no front-end code change.
        .route("/v1/tools", get(routes::list_available_tools))
        // Dashboard management API
        .merge(dashboard::router())
        //TickLoop toggle
        .route("/v1/tickloop", get(voice::get_tickloop_status).post(voice::toggle_tickloop))
        // L3: Voice bridge — HTTP-friendly STT endpoint. The route is
        // stateless: the bridge builds a default `VoicePipeline` per
        // request so concurrent calls don't share config state. The
        // `voice_mode` tool (LLM-invoked) is the proper way to drive a
        // long-lived pipeline with custom STT/TTS providers.
        .route("/v1/voice/transcribe", post(voice::voice_transcribe))
        .nest_service("/dashboard-plugins", ServeDir::new(plugin_dir))
        .with_state(state);

    // Optional SPA static hosting (`HERMES_SERVE_WEB_STATIC=0` disables for Vercel-split deploy).
    let serve_web_static = !matches!(
        env_compat("HERMIX_SERVE_WEB_STATIC", "HERMES_SERVE_WEB_STATIC").as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    );

    if serve_web_static {
        // Serve the dashboard SPA from the `apps/dashboard/dist` directory if it exists.
        // Checks HERMES_WEB_DIST env var first, then common relative paths.
        let web_dist = env_compat("HERMIX_WEB_DIST", "HERMES_WEB_DIST")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let candidates = [
                    std::path::PathBuf::from("apps/dashboard/dist"),
                    std::path::PathBuf::from("../apps/dashboard/dist"),
                    std::path::PathBuf::from("../../apps/dashboard/dist"),
                ];
                candidates
                    .into_iter()
                    .find(|p| p.join("index.html").exists())
            });

        if let Some(dist_dir) = web_dist {
            let index_html = dist_dir.join("index.html");
            if index_html.exists() {
                tracing::info!("Serving web dashboard from {}", dist_dir.display());
                // SPA: static files from `dist/`, and missing paths fall back to `index.html`.
                // Use `fallback`, not `not_found_service` — the latter wraps responses in
                // `SetStatus(404)` per tower-http, which breaks client-side routes (browser shows
                // "404" even when HTML is present).
                let index_nf = Arc::new(index_html);
                let spa = ServeDir::new(&dist_dir).fallback(service_fn(
                    move |_req: axum::http::Request<Body>| {
                        let p = index_nf.clone();
                        async move {
                            let bytes = tokio::fs::read(&*p).await.unwrap_or_default();
                            let res = Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                                .body(Body::from(bytes))
                                .unwrap();
                            Ok::<_, Infallible>(res)
                        }
                    },
                ));
                app = app.fallback_service(spa);
            }
        }
    } else {
        tracing::info!("HERMES_SERVE_WEB_STATIC disables bundled SPA static hosting");
    }

    app.layer(middleware::from_fn(move |req, next| {
        let sec = sec_guard.clone();
        let rl = rate_guard.clone();
        let dr = device_registry.clone();
        async move { security::request_guard(sec, rl, dr, req, next).await }
    }))
    .layer(tower_http::limit::RequestBodyLimitLayer::new(body_limit))
    // Defense in depth for the request trace: a client (or a third-party
    // page hitting this origin) can still put `?token=` / `?api_key=` in a
    // URL, and the default span records the raw query string into the log
    // and any shipped bundle. The server no longer *accepts* a credential
    // from the query — this only stops one from being logged.
    //
    // tower-http 0.6 ships only `DefaultMakeSpan` (no `MakeSpanRef`), so
    // the redaction rides on `on_request`; the span's own `uri` field is
    // not overridable. `include_headers(false)` keeps request headers out.
    // (The agent-side `pii_redact` does NOT cover this — it post-processes
    // tool results, not the server's own trace output.)
    .layer(
        tower_http::trace::TraceLayer::new_for_http()
            .make_span_with(
                tower_http::trace::DefaultMakeSpan::new()
                    .level(tracing::Level::INFO)
                    .include_headers(false),
            )
            .on_request({
                #[allow(clippy::redundant_closure)]
                |req: &axum::http::Request<axum::body::Body>, span: &tracing::Span| {
                    span.record("redacted_uri", tracing::field::debug(&redact_query_credentials(req.uri())));
                }
            }),
    )
    .layer(http_cors_layer())
}

pub async fn run_server(addr: SocketAddr, config: GatewayConfig) -> Result<(), AgentError> {
    // 安全中心：按 config 的 security 刷新自动备份策略（与 main.rs 启动路径一致）。
    hermix_tools::backup::set_policy_from_config(&config.security, &hermix_config::paths::hermix_home());
    let state = HttpServerState::build(config)
        .await?
        .with_automation_scheduler()
        // 启动 relay 出站连接。未配地址时它自己直接返回，不产生任何流量。
        .with_relay_client();
    // §5.5 第 2 层：TLS 监听器由 HERMIX_TLS_ADDR 显式开启（可选双监听器：
    // plaintext 保持现有暴露面语义，TLS 面向远程设备）。env 读法放在这里
    // 而不是 main.rs，是为了 run_server 的调用方零改动。
    let tls_addr = tls::tls_addr_from_env();
    run_server_with_tls(addr, tls_addr, state).await
}

pub async fn run_server_with_state(
    addr: SocketAddr,
    state: HttpServerState,
) -> Result<(), AgentError> {
    run_server_with_tls(addr, None, state).await
}

pub async fn run_server_with_tls(
    addr: SocketAddr,
    tls_addr: Option<SocketAddr>,
    state: HttpServerState,
) -> Result<(), AgentError> {
    //clone TickLoop manager before moving state into the router.
    let tickloop_mgr = state.tickloop_manager.clone();
    let tracker = Arc::new(state.consciousness_tracker.clone());

    // Clone agent_service + active_handles BEFORE moving `state` into
    // the router. On shutdown we call `interrupt_all()` so active agent
    // tasks gracefully stop and persist their partial output — without
    // this, spawned tasks are dropped mid-stream when the process exits
    // and the session reloads empty on restart.
    let agent_service_for_shutdown = state.agent_service.clone();
    let active_handles_for_shutdown = state.active_handles.clone();

    // IM 通道（QQ / 企业微信官方机器人）。env 门控：凭据成对齐全才启。
    // 它自己出站连 IM 网关、不占本机端口，所以和是否开远程访问无关——
    // 这也意味着它的安全边界完全落在配对码 + allowlist 上，不在监听器上。
    let im_runtime = channels::ImRuntime::start(state.clone());
    // 登记进全局槽位：绑定/解绑后凭据变化可就地热重启通道，
    // 不必让用户杀应用重开（见 channels::restart_runtime）。
    channels::install_runtime(im_runtime.clone());

    // Shutdown signal: /shutdown endpoint sends `true`, graceful shutdown waits for it.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let app = router(state).layer(axum::Extension(shutdown_tx));
    //fix: bind via TcpSocket so we can set SO_REUSEADDR
    // before bind. Default TcpListener::bind does NOT set it on
    // Windows, which means a crashed/exited child leaving a LISTEN
    // socket on 3000 locks the port until the OS reaps the orphan
    // (30s~4min). With SO_REUSEADDR we can re-bind immediately even
    // if the previous listener is still in TIME_WAIT / CLOSE_WAIT.
    // On Linux this is also helpful for fast-restart during dev.
    // Tokio API: TcpSocket::bind() returns (), then socket.listen(1024)
    // returns the actual TcpListener. This is two-step by design so
    // SO_REUSEADDR/SO_REUSEPORT can be set between bind and listen.
    let socket = tokio::net::TcpSocket::new_v4()
        .map_err(|e| AgentError::Io(format!("TcpSocket::new_v4: {e}")))?;
    socket
        .set_reuseaddr(true)
        .map_err(|e| AgentError::Io(format!("set_reuseaddr: {e}")))?;
    // Best-effort: SO_REUSEPORT (Linux/macOS) — allows multiple
    // processes to bind the same port for load-balancing.
    // Silently ignored on Windows where it doesn't exist.
    #[cfg(unix)]
    {
        let _ = socket.set_reuseport(true);
    }
    socket.bind(addr).map_err(|e| {
        AgentError::Io(format!(
            "bind {addr} failed: {e} (port may still be held by a crashed previous process; retry after ~30s or pick a different port)"
        ))
    })?;
    let listener = socket.listen(1024).map_err(|e| {
        AgentError::Io(format!("listen on {addr}: {e}"))
    })?;
    tracing::info!("hermes-server listening on {} (SO_REUSEADDR=on)", addr);

    // Start TickLoop by default (powers conscious ECG, watchdog).
    tickloop_mgr.start(Some(tracker)).await;

    let mut shutdown_rx = shutdown_rx;
    // TLS 联动用副本：shutdown 闭包 move 走原 rx。
    let shutdown_rx_for_tls = shutdown_rx.clone();
    let shutdown = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("hermes-server graceful shutdown (ctrl_c)");
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("hermes-server graceful shutdown (HTTP /shutdown)");
            }
        }
        // Interrupt all active agent tasks so they gracefully stop
        // and persist their partial output before the process exits.
        // Without this, spawned agent tasks are dropped mid-stream
        // when the runtime shuts down, and the session reloads empty.
        agent_service_for_shutdown.interrupt_all();
        // Give agent tasks up to 2s to finish persisting. The sidecar's
        // stop() waits 3s for graceful exit, so 2s here leaves ~1s of
        // slack for axum connection teardown + tickloop stop.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let remaining = {
                match active_handles_for_shutdown.lock() {
                    Ok(guard) => guard.len(),
                    Err(_) => 0,
                }
            };
            if remaining == 0 || std::time::Instant::now() >= deadline {
                if remaining > 0 {
                    tracing::warn!(
                        active_tasks = remaining,
                        "shutdown: {} agent task(s) did not finish within 2s — their partial output may be lost",
                        remaining
                    );
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        tickloop_mgr.stop().await;
        if let Some(rt) = im_runtime.as_ref() {
            rt.stop().await;
        }
    };

    // §5.5 第 2 层：可选 TLS 监听器。plaintext 主监听器行为不变；
    // TLS listener 用同一 router（Router: Clone），shutdown 联动。
    if let Some(tls_addr) = tls_addr {
        let dir = tls::tls_dir();
        let material = tls::ensure_tls_material(&dir)?;
        let server_config = tls::build_server_config(&material)?;
        tracing::info!(
            "TLS listener on {tls_addr}, cert sha256={} (TOFU 指纹固定锚点)",
            material.fingerprint_sha256
        );
        drop(material);

        // 复用 SO_REUSEADDR socket 装配（与 plaintext 相同的两步法）。
        let tls_socket = tokio::net::TcpSocket::new_v4()
            .map_err(|e| AgentError::Io(format!("TcpSocket::new_v4(tls): {e}")))?;
        tls_socket
            .set_reuseaddr(true)
            .map_err(|e| AgentError::Io(format!("set_reuseaddr(tls): {e}")))?;
        tls_socket.bind(tls_addr).map_err(|e| {
            AgentError::Io(format!("tls bind {tls_addr} failed: {e}"))
        })?;
        let tls_listener = tls_socket.listen(1024).map_err(|e| {
            AgentError::Io(format!("tls listen on {tls_addr}: {e}"))
        })?;
        let std_listener = tls_listener
            .into_std()
            .map_err(|e| AgentError::Io(format!("tls into_std: {e}")))?;
        std_listener
            .set_nonblocking(false)
            .map_err(|e| AgentError::Io(format!("tls set_nonblocking: {e}")))?;

        let shutdown_tls_handle = axum_server::Handle::new();
        let rustls_config =
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(server_config));
        let tls_server = axum_server::tls_rustls::from_tcp_rustls(
            std_listener,
            rustls_config,
        )
        .handle(shutdown_tls_handle.clone())
        .serve(app.clone().into_make_service_with_connect_info::<SocketAddr>());

        // shutdown 联动：watch 通道变化 → TLS listener 优雅退出。
        let mut shutdown_rx_tls = shutdown_rx_for_tls.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx_tls.changed().await;
            shutdown_tls_handle.graceful_shutdown(Some(std::time::Duration::from_secs(2)));
        });

        return tokio::select! {
            r = async {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(shutdown)
                .await
                .map_err(|e| AgentError::Io(e.to_string()))
            } => r,
            r = async {
                tls_server
                    .await
                    .map_err(|e| AgentError::Io(e.to_string()))
            } => r,
        };
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| AgentError::Io(e.to_string()))
}

/// 浏览器小窗暂停/恢复。桌面端小窗最小化/遮挡时调用：真正停/恢复的是 sidecar
/// 里 headless Edge 的 screencast 编码（CPU 闸门），不是桌面进程里的什么——
/// 桌面进程没有浏览器会话，调用它自己的 `screencast::pause()` 只会静默 no-op。
async fn browser_view_paused(Json(body): Json<serde_json::Value>) -> impl IntoResponse {
    let paused = body
        .get("paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if paused {
        hermix_tools::tools::os_auto::browser::screencast::pause().await;
    } else {
        hermix_tools::tools::os_auto::browser::screencast::resume().await;
    }
    (
        [("content-type", "application/json")],
        "{\"ok\":true}",
    )
        .into_response()
}

async fn health() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        timestamp: Utc::now().to_rfc3339(),
    })
}

async fn shutdown_server(
    axum::extract::Extension(tx): axum::extract::Extension<tokio::sync::watch::Sender<bool>>,
) -> impl IntoResponse {
    let _ = tx.send(true);
    "shutting down"
}

// ---------------------------------------------------------------------------
// Expert catalog — backed by `HttpServerState::expert_registry`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod probe_url_tests {
    use super::*;
    use hermix_config::LlmProviderConfig;
    use std::collections::HashMap;

    fn cfg_with(model: Option<&str>, providers: &[(&str, &str)]) -> GatewayConfig {
        let mut c = GatewayConfig::default();
        c.model = model.map(|s| s.to_string());
        let mut map = HashMap::new();
        for (k, u) in providers {
            map.insert(
                (*k).to_string(),
                LlmProviderConfig {
                    base_url: Some((*u).to_string()),
                    ..Default::default()
                },
            );
        }
        c.llm_providers = map;
        c
    }

    fn cfg_with_key(model: Option<&str>, providers: &[(&str, &str)]) -> GatewayConfig {
        let mut c = GatewayConfig::default();
        c.model = model.map(|s| s.to_string());
        let mut map = HashMap::new();
        for (k, api_key) in providers {
            map.insert(
                (*k).to_string(),
                LlmProviderConfig {
                    api_key: Some((*api_key).to_string()),
                    ..Default::default()
                },
            );
        }
        c.llm_providers = map;
        c
    }

    #[test]
    fn extract_from_active_model() {
        let c = cfg_with(Some("minimax:MiniMax-M2"), &[("minimax", "https://api.MiniMax.chat/v1")]);
        let (name, url) = extract_llm_probe_url(&c).unwrap();
        assert_eq!(name, "minimax");
        assert_eq!(url, Some("https://api.MiniMax.chat/v1".to_string()));
    }

    #[test]
    fn fallback_to_first_with_url() {
        let c = cfg_with(None, &[("openai", "https://api.openai.com/v1")]);
        let (name, url) = extract_llm_probe_url(&c).unwrap();
        assert_eq!(name, "openai");
        assert_eq!(url, Some("https://api.openai.com/v1".to_string()));
    }

    #[test]
    fn key_only_returns_name_no_url() {
        // When only api_key is set (no base_url), we should still return
        // the provider name so the runtime can infer a default URL.
        let c = cfg_with_key(None, &[("minimax", "sk-test-key-123")]);
        let (name, url) = extract_llm_probe_url(&c).unwrap();
        assert_eq!(name, "minimax");
        assert_eq!(url, None); // base_url is not set, but runtime can infer it
    }

    #[test]
    fn none_when_empty() {
        let c = GatewayConfig::default();
        assert_eq!(extract_llm_probe_url(&c), None);
    }
}

/// SO_REUSEADDR regression test. Simulates the exact
/// "crashed child left a LISTEN socket on the same port" scenario
/// that crashed hermix-desktop sidecar on Windows with
/// `os error 10048 (WSAEADDRINUSE)`.
/// We:
/// 1. Bind a TCP listener on a random port with SO_REUSEADDR=on
/// (mirroring the production TcpSocket::bind path in
/// `run_server_with_state`).
/// 2. Drop that listener without graceful shutdown — simulating
/// a process crash.
/// 3. Immediately re-bind the SAME port with SO_REUSEADDR=on.
/// 4. Assert the second bind succeeds (would fail with
/// WSAEADDRINUSE on Windows without SO_REUSEADDR).
#[cfg(test)]
mod reuseaddr_regression_tests {
    use std::net::SocketAddr;

    #[tokio::test]
    async fn rebind_after_crash_works_on_same_port() {
        // Pick an ephemeral port (0) for isolation — OS assigns one.
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

        // First listener (simulates running server).
        let sock1 = tokio::net::TcpSocket::new_v4().unwrap();
        sock1.set_reuseaddr(true).unwrap();
        sock1.bind(addr).unwrap();
        let listener1 = sock1.listen(16).unwrap();
        let bound_addr = listener1.local_addr().unwrap();

        // Simulate crash: drop without graceful shutdown. On Windows
        // this leaves the LISTEN socket as an orphan; without
        // SO_REUSEADDR, the next bind to the same port fails with
        // os error 10048.
        drop(listener1);

        // Small grace period — not strictly required with
        // SO_REUSEADDR, but mirrors a real "user clicks Restart
        // 200ms after crash" gap.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Second listener (simulates hermix restart).
        let sock2 = tokio::net::TcpSocket::new_v4()
            .expect("TcpSocket::new_v4 should succeed");
        sock2
            .set_reuseaddr(true)
            .expect("set_reuseaddr should succeed");
        sock2
            .bind(bound_addr)
            .unwrap_or_else(|e| panic!("rebind {bound_addr} failed (SO_REUSEADDR bug?): {e}"));
        let listener2 = sock2.listen(16).expect("listen on rebound port");

        // Sanity: new listener is on the same port.
        assert_eq!(listener2.local_addr().unwrap(), bound_addr);
    }

    #[test]
    fn reuseaddr_can_be_set_without_panic() {
        // Pure sync sanity — set_reuseaddr on a freshly-built
        // TcpSocket should never panic.
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.set_reuseaddr(true).unwrap();
    }
}

/// regression coverage for the
/// "database disk image is malformed" `delete_session` bug.
/// Root cause: `delete_session` opened `sessions.db` in the legacy
/// `journal_mode = delete` (single-writer, blocking) default while
/// the writer side ran in `journal_mode = WAL`. When the writer
/// was killed mid-transaction (Tauri sidecar SIGKILL on shutdown,
/// for example), the `-shm` page table was left dirty and the next
/// non-WAL opener surfaced the "malformed" diagnostic. Setting
/// `journal_mode = WAL` on the delete side, plus a passive
/// pre-checkpoint and a post-commit truncate, makes both processes
/// speak the same journal format and keeps the on-disk file
/// fully consistent.
/// This test asserts (1) the delete-side PRAGMA matches the
/// writer side, (2) the WAL is truncated after commit, and
/// (3) the 404 path rolls back the cascade DELETEs.
/// extension : the WAL regression tests
/// above pin the journal_mode contract. This test pins the FTS5
/// shadow-page corruption recovery path: pre-corrupt the FTS index
/// by deleting shadow rows, then verify delete_session rebuilds
/// and still completes the cascade.
#[cfg(test)]
mod delete_session_fts_recovery_tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn seed_db_with_fts_corrupted() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT, message_count INTEGER);
             CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, created_at TEXT, active INTEGER DEFAULT 1);
             CREATE TABLE compression_archives (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, content TEXT NOT NULL, original_turns INTEGER, compressed_turns INTEGER, original_tokens INTEGER, compressed_tokens INTEGER, salience REAL, source TEXT, created_at TEXT);
             CREATE TABLE compression_summary_history (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, summary TEXT NOT NULL, compression_count INTEGER, original_turns INTEGER, compressed_turns INTEGER, original_tokens INTEGER, compressed_tokens INTEGER, tokens_saved INTEGER, created_at TEXT);
             CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, model TEXT, prompt_tokens INTEGER, completion_tokens INTEGER, total_tokens INTEGER, estimated_cost REAL, created_at TEXT);
             CREATE VIRTUAL TABLE messages_fts USING fts5(content, session_id UNINDEXED, content=messages, content_rowid=id);
             CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN INSERT INTO messages_fts(rowid, content, session_id) VALUES (new.id, new.content, new.session_id); END;
             CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, content, session_id) VALUES('delete', old.id, old.content, old.session_id); END;
             CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, content, session_id) VALUES('delete', old.id, old.content, old.session_id); INSERT INTO messages_fts(rowid, content, session_id) VALUES (new.id, new.content, new.session_id); END;",
        ).unwrap();
        // Seed a session and a message.
        conn.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at, message_count) VALUES ('fts-test', 'corrupt', '2026-01-01', '2026-01-01', 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES ('fts-test', 'user', 'hello world', '2026-01-01')",
            [],
        ).unwrap();
        // Manually corrupt the FTS shadow by deleting a rowid that
        // the messages table still references. This breaks the
        // invariant the messages_ad trigger depends on, so a
        // subsequent DELETE FROM messages will error with
        // 'database disk image is malformed' on the trigger.
        conn.execute("DELETE FROM messages_fts WHERE rowid = 1", []).unwrap();
        // Sanity: a normal DELETE should now fail.
        let bad = conn.execute("DELETE FROM messages WHERE session_id = 'fts-test'", []);
        assert!(bad.is_err(), "FTS corruption setup didn't break DELETE");
        (dir, db)
    }

    #[test]
    fn delete_session_recovers_from_corrupt_fts() {
        let (_dir, db) = seed_db_with_fts_corrupted();
        let conn = routes::open_sessions_db_for_read(&db).expect("open");

        // The DELETE itself should trigger the malformed error path
        // and our helper should rebuild the FTS index + retry.
        // (We test the helper directly because the actual
        // `delete_session` async fn is hard to drive from a unit
        // test without spinning up the full axum router.)
        let tx = conn.unchecked_transaction().unwrap();
        let res = tx.execute("DELETE FROM messages WHERE session_id = 'fts-test'", []);
        assert!(res.is_err(), "expected malformed error");
        assert!(routes::is_malformed_error(&res.unwrap_err()));
        let _ = tx.rollback();

        // Rebuild and retry — this is exactly what delete_session
        // does on the recovery path.
        routes::rebuild_messages_fts(&conn).expect("rebuild");
        let tx2 = conn.unchecked_transaction().unwrap();
        tx2.execute("DELETE FROM messages WHERE session_id = 'fts-test'", []).unwrap();
        tx2.execute("DELETE FROM sessions WHERE id = 'fts-test'", []).unwrap();
        tx2.commit().unwrap();
    }

    #[test]
    fn non_malformed_errors_are_not_swallowed() {
        // A "no such table" error is NOT malformed-shaped; the
        // helper must return false so the caller surfaces 500
        // instead of attempting an unnecessary FTS rebuild.
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("no such table: foo".to_string()),
        );
        assert!(!routes::is_malformed_error(&err));
    }
}

/// Existing WAL regression suite (kept intact).
#[cfg(test)]
mod delete_session_wal_regression_tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn seed_sessions_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, created_at TEXT);
             CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT);
             CREATE TABLE compression_archives (id INTEGER PRIMARY KEY, session_id TEXT);
             CREATE TABLE compression_summary_history (id INTEGER PRIMARY KEY, session_id TEXT);
             CREATE TABLE token_usage (id INTEGER PRIMARY KEY, session_id TEXT);",
        )
        .unwrap();
        for sid in ["keep-me", "drop-me", "also-keep"] {
            conn.execute(
                "INSERT INTO sessions (id, title, created_at) VALUES (?1, ?2, '2026-07-07T00:00:00Z')",
                rusqlite::params![sid, format!("title {sid}")],
            )
            .unwrap();
            // Orphan message rows in every cascade table.
            for table in ["messages", "compression_archives", "compression_summary_history", "token_usage"] {
                conn.execute(
                    &format!("INSERT INTO {table} (session_id) VALUES (?1)"),
                    rusqlite::params![sid],
                )
                .unwrap();
            }
        }
    }

    fn count_table(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// Mirrors the production `delete_session` PRAGMA sequence so
    /// the test catches drift if the production code is changed.
    fn open_for_delete(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .unwrap();
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL;");
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        conn
    }

    #[test]
    fn delete_session_uses_wal_journal_mode() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions_db(&db);
        // Writer side set WAL — the new opener must match.
        let conn = open_for_delete(&db);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "delete side must use WAL");
    }

    #[test]
    fn delete_cascades_to_child_tables_and_removes_session_row() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions_db(&db);
        let conn = open_for_delete(&db);
        let tx = conn.unchecked_transaction().unwrap();
        for table in [
            "messages",
            "compression_archives",
            "compression_summary_history",
            "token_usage",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE session_id = 'drop-me'"),
                [],
            )
            .unwrap();
        }
        let n = tx
            .execute("DELETE FROM sessions WHERE id = 'drop-me'", [])
            .unwrap();
        assert_eq!(n, 1);
        tx.commit().unwrap();
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        // After commit the row is gone everywhere.
        assert_eq!(count_table(&conn, "sessions"), 2);
        for table in [
            "messages",
            "compression_archives",
            "compression_summary_history",
            "token_usage",
        ] {
            let total = count_table(&conn, table);
            // 2 surviving sessions × 1 child row each = 2.
            assert_eq!(total, 2, "table {table} should be cascaded");
        }
    }

    #[test]
    fn delete_missing_session_returns_zero_and_rolls_back() {
        // Mirrors the 404 path: if the session row isn't there, the
        // handler must NOT leave the child-table DELETEs committed
        // (they're no-ops, but the rollback is the safe default).
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions_db(&db);
        let conn = open_for_delete(&db);
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "DELETE FROM messages WHERE session_id = 'no-such-session'",
            [],
        )
        .unwrap();
        let n = tx
            .execute("DELETE FROM sessions WHERE id = 'no-such-session'", [])
            .unwrap();
        assert_eq!(n, 0);
        let _ = tx.rollback();
        // Nothing should be gone after rollback.
        assert_eq!(count_table(&conn, "sessions"), 3);
        assert_eq!(count_table(&conn, "messages"), 3);
    }

    #[test]
    fn wal_is_truncated_after_commit() {
        // After `wal_checkpoint(TRUNCATE)` the `-wal` sidecar should
        // be empty. We assert by checking the file size shrinks to 0
        // (the `-shm` file remains — it's the page table).
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions_db(&db);
        let conn = open_for_delete(&db);
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute("DELETE FROM messages WHERE session_id = 'drop-me'", [])
            .unwrap();
        tx.execute("DELETE FROM sessions WHERE id = 'drop-me'", [])
            .unwrap();
        tx.commit().unwrap();
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        let wal_path = dir.path().join("sessions.db-wal");
        if wal_path.exists() {
            let len = std::fs::metadata(&wal_path).unwrap().len();
            assert_eq!(len, 0, "WAL should be truncated to 0 bytes");
        }
        // Reopen and integrity-check — proves we left a clean DB.
        let reopened = Connection::open(&db).unwrap();
        let integrity: String = reopened
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    /// Every server route that touches `sessions.db` must go through
    /// `open_sessions_db_for_read` so the journal mode matches the
    /// writer side. This test pins that contract: a route that opens
    /// the db without the helper gets `journal_mode = delete`, the
    /// helper forces WAL, and we assert the helper wins.
    #[test]
    fn open_sessions_db_for_read_sets_wal_journal_mode() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions_db(&db);

        // Sanity: plain `Connection::open` keeps the legacy default
        // (`delete`) on a fresh file. We don't pin it here (SQLite's
        // journal mode can persist in the file header after a prior
        // WAL open, so it's not a reliable differentiator). What
        // matters is the helper below forces WAL regardless.
        let _raw = Connection::open(&db).unwrap();

        let conn = routes::open_sessions_db_for_read(&db).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let _: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
    }

    #[test]
    fn list_sessions_does_not_panic_when_db_missing() {
        // Regression: list_sessions used to swallow open errors and
        // return an empty list. Now it goes through
        // `open_sessions_db_for_read`. `Connection::open` on a missing
        // file path CREATES the file (rusqlite default), so the helper
        // succeeds — but the file has **no schema**, so any
        // `SELECT FROM sessions` raises "no such table: sessions".
        // The pre-fix behavior silently swallowed this error and
        // returned an empty list. This test pins the helper's contract:
        // the open succeeds and the journal mode is WAL, but the
        // SELECT is the caller's responsibility to handle gracefully
        // (every route does — they all use `unwrap_or_default()` on
        // the query_map iterator).
        let dir = tempdir().unwrap();
        let db = dir.path().join("does-not-exist.db");
        let conn = routes::open_sessions_db_for_read(&db).expect("missing db should auto-create");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "fresh db must still get WAL");
        // Empty DB — the SELECT against an un-migrated schema errors,
        // but each route wraps the query in `unwrap_or_default()`,
        // so the user sees an empty list rather than a 500. Pin that
        // contract here so a future refactor doesn't accidentally
        // turn it into a 500.
        let result: rusqlite::Result<i64> =
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0));
        assert!(
            result.is_err(),
            "empty db must report 'no such table' so callers can degrade"
        );
    }

    /// 会话列表必须按归属切分：默认只给普通对话，传了 workbench_id 只给
    /// 那一个工作台的。改回无条件 `SELECT ... ORDER BY` 就等于把工作台的
    /// 对话摆回主侧栏 —— 用户点进去拿到一段脱离工作台上下文的历史，两个
    /// 对话空间重新串成一条。这里钉住 SQL 的形状，因为过滤发生在 SQL 里
    /// 而不是 Rust 里，类型系统管不到。
    #[test]
    fn session_list_is_scoped_by_workbench_ownership() {
        // 钉住的是常量的**值**，不是「源码里出现过这个字符串」——后者是
        // 自指：字面量就写在常量定义里，怎么改都成立（试过把值换成
        // `1 = 1`，那条断言照样绿）。值只能在这里硬编码期望。
        assert_eq!(
            routes::DEFAULT_SESSION_SCOPE_SQL,
            "workbench_id IS NULL",
            "default scope must exclude workbench-bound sessions, \
             otherwise their conversations leak into the main sidebar"
        );
        assert_eq!(
            routes::WORKBENCH_SESSION_SCOPE_SQL,
            "workbench_id = ?1",
            "workbench scope must be a bound parameter, not string interpolation"
        );
        // 再确认 SQL 真的引用了这两个常量 —— 不然有人可以绕开它们
        // 直接把条件写死在语句里，上面两条就形同虚设。
        let src = include_str!("routes.rs");
        let default_use = concat!("{DEFAULT", "_SESSION_SCOPE_SQL}");
        let workbench_use = concat!("{WORKBENCH", "_SESSION_SCOPE_SQL}");
        assert!(
            src.contains(default_use),
            "list_sessions stopped using DEFAULT_SESSION_SCOPE_SQL"
        );
        assert!(
            src.contains(workbench_use),
            "list_sessions stopped using WORKBENCH_SESSION_SCOPE_SQL"
        );
    }

    /// 归属必须是可写的，而且只能经由 PATCH 写 —— 桌面端直接开 sessions.db
    /// 会和 sidecar 抢写锁。这两条一起保证「会话归谁」有唯一入口。
    #[test]
    fn workbench_id_is_writable_through_patch_only() {
        let routes_src = include_str!("routes.rs");
        assert!(
            routes_src.contains("UPDATE sessions SET workbench_id = ?1"),
            "PATCH /v1/sessions can no longer set workbench_id — \
             sessions cannot be assigned to a workbench"
        );
        let plugin_src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop-tauri/src-tauri/src/commands_workbench.rs"
        ));
        assert!(
            !plugin_src.contains("sessions.db"),
            "desktop code must not open sessions.db directly — \
             it would race the sidecar's own writes; go through PATCH"
        );
    }
}


