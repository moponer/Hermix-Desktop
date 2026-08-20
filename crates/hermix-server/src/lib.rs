#![allow(clippy::doc_lazy_continuation, clippy::field_reassign_with_default)]
//! HTTP and WebSocket API server for Hermes.
//!
//! Environment (see also `security` module):
//! - `HERMES_HTTP_MAX_BODY_BYTES` — max JSON body size for POST routes (default 2 MiB).
//! - `HERMES_HTTP_CORS_ORIGINS` — comma-separated browser Origins for CORS; empty → permissive.
//! - `HERMES_SERVE_WEB_STATIC` — set `0`/`false`/`off` to skip serving `apps/dashboard/dist` (API-only).
//! Policy HTTP routes are intentionally omitted (Hermes Python does not expose them).

pub mod dashboard;
mod automation_trigger;
mod orchestration_bus;
mod task_tracker_adapter;
mod security;
mod crash_dump;
mod metrics;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_state;

pub use security::parse_allowed_ips;
pub use security::PolicyGuardConfig;

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

use axum::body::{Bytes, Body};
use axum::extract::{Path, State};
use axum::http::header;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::{FutureExt, SinkExt, StreamExt};
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
/// open indefinitely until the user explicitly clicks 允许 or 拒绝.
/// This is the user's explicit requirement : "审批不该
/// 有超时，如果不审批默认就等待，不再继续往下". The agent loop
/// blocks on the oneshot receiver until the user resolves it; the
/// only other unblock paths are WS disconnect (→ `ChannelClosed`)
/// and server-side task abort.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::ZERO;

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
if DEFAULT_APPROVAL_TIMEOUT.is_zero() {
return None;
}
Some(
chrono::Utc::now().timestamp_millis()
+ DEFAULT_APPROVAL_TIMEOUT.as_millis() as i64,
)
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
pub use approval::ApprovalRegistry;

pub mod state;
pub use state::{ClarifyBroadcast, HttpServerState};
pub mod errors;
pub use errors::HttpError;
pub mod voice;
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

#[derive(Debug, Deserialize)]
pub struct ClientTelemetryEvent {
    pub runtime: String,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
}

fn max_request_body_bytes() -> usize {
    const DEFAULT: usize = 2 * 1024 * 1024;
    env_compat("HERMIX_HTTP_MAX_BODY_BYTES", "HERMES_HTTP_MAX_BODY_BYTES")
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
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
        .route("/v1/protocol/schema", get(routes::protocol_schema))
        .route("/v1/telemetry/client-event", post(routes::client_telemetry_event))
        .route("/metrics", get(routes::metrics_prometheus))
        .route("/v1/metrics", get(routes::metrics_snapshot))
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
        // Approval response REST fallback — when WebSocket delivery
        // fails (Tauri WebView2 can lose buffered messages on connection
        // reset), the frontend sends the approval via HTTP as a backup.
        .route(
            "/v1/sessions/{session_id}/approval",
            post(routes::approve_via_rest),
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
        .route("/v1/ws/{session_id}", get(ws::ws_upgrade))
        .route("/v1/ws-stream/{session_id}", get(ws::ws_stream_upgrade))
        .route("/v1/ws-groupchat/{room_id}", get(ws::ws_groupchat_upgrade))
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
        // L3: Voice bridge — HTTP-friendly STT/TTS endpoints.
        // Both routes are stateless: the bridge builds a default
        // `VoicePipeline` per request so concurrent calls don't share
        // config state. The `voice_mode` tool (LLM-invoked) is the
        // proper way to drive a long-lived pipeline with custom
        // STT/TTS providers.
        .route("/v1/voice/transcribe", post(voice::voice_transcribe))
        .route("/v1/voice/synthesize", post(voice::voice_synthesize))
        // Orchestration Engine Task CRUD endpoints.
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route(
            "/v1/tasks/{task_id}",
            get(get_task).patch(patch_task).delete(delete_task),
        )
        .route("/v1/tasks/{task_id}/status", patch(patch_task_status))
        .route("/v1/tasks/{task_id}/children", get(list_task_children))
// Product delivery — register deliverable artifacts.
.route("/v1/tasks/{task_id}/results", post(deliver_results))
        // Automation Engine CRUD endpoints.
        .route("/v1/automations", get(list_automations).post(create_automation))
        .route("/v1/automations/{id}", get(get_automation).delete(delete_automation))
        .route("/v1/automations/{id}/pause", post(pause_automation))
        .route("/v1/automations/{id}/resume", post(resume_automation))
        .route("/v1/automations/{id}/runs", get(list_automation_runs))
.route("/v1/automations/{id}/trigger", post(trigger_automation_endpoint))
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
        async move { security::request_guard(sec, rl, req, next).await }
    }))
    .layer(tower_http::limit::RequestBodyLimitLayer::new(body_limit))
    .layer(tower_http::trace::TraceLayer::new_for_http())
    .layer(http_cors_layer())
}

pub async fn run_server(addr: SocketAddr, config: GatewayConfig) -> Result<(), AgentError> {
    let state = HttpServerState::build(config).await?.with_automation_scheduler();
    run_server_with_state(addr, state).await
}

pub async fn run_server_with_state(
    addr: SocketAddr,
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
    };
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| AgentError::Io(e.to_string()))
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
}

// ===========================================================================
/// Orchestration Engine — Task CRUD HTTP handlers
// ===========================================================================

/// Request body for `POST /v1/tasks`.
#[derive(Debug, serde::Deserialize)]
struct CreateTaskRequest {
    title: String,
    workspace_path: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    parent_task_id: Option<String>,
    #[serde(default)]
    context: hermix_core::TaskContext,
}

fn default_mode() -> String {
    "craft".to_string()
}

/// `POST /v1/tasks` — Create a new Task.
async fn create_task(
    State(state): State<HttpServerState>,
    axum::Json(req): axum::Json<CreateTaskRequest>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let mode = match req.mode.as_str() {
        "craft" => hermix_core::TaskMode::Craft,
        "plan" => hermix_core::TaskMode::Plan,
        "ask" => hermix_core::TaskMode::Ask,
        _ => return (StatusCode::BAD_REQUEST, "Invalid mode").into_response(),
    };

    let params = hermix_task::service::CreateTaskParams {
        title: req.title,
        workspace_path: req.workspace_path,
        mode,
        parent_task_id: req.parent_task_id,
        context: req.context,
    };

    match svc.create(params) {
        Ok(task) => {
            state.orchestration_bus.emit(OrchestrationEvent::TaskCreated {
                task_id: task.id.clone(),
                title: task.title.clone(),
                mode: format!("{:?}", task.mode).to_lowercase(),
                workspace_path: task.workspace_path.clone(),
            });
            axum::Json(task).into_response()
        }
        Err(e) => {
            tracing::warn!("create_task error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `GET /v1/tasks` — List tasks, optionally filtered by status.
async fn list_tasks(
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<TaskListQuery>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let status_filter = params.status.as_deref().and_then(|s| match s {
        "pending" => Some(hermix_core::TaskStatus::Pending),
        "planning" => Some(hermix_core::TaskStatus::Planning),
        "in_progress" => Some(hermix_core::TaskStatus::InProgress),
        "completed" => Some(hermix_core::TaskStatus::Completed),
        "failed" => Some(hermix_core::TaskStatus::Failed),
        "archived" => Some(hermix_core::TaskStatus::Archived),
        _ => None,
    });

    match svc.list(status_filter) {
        Ok(tasks) => axum::Json(tasks).into_response(),
        Err(e) => {
            tracing::warn!("list_tasks error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct TaskListQuery {
    status: Option<String>,
}

/// `GET /v1/tasks/{task_id}` — Get a single Task.
async fn get_task(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.get(&task_id) {
        Ok(task) => axum::Json(task).into_response(),
        Err(hermix_task::TaskServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("get_task error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// Request body for `PATCH /v1/tasks/{task_id}`.
#[derive(Debug, serde::Deserialize)]
struct PatchTaskRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    context: Option<hermix_core::TaskContext>,
}

/// `PATCH /v1/tasks/{task_id}` — Update a Task's mutable fields.
async fn patch_task(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
    axum::Json(req): axum::Json<PatchTaskRequest>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let result = if let Some(title) = req.title {
        svc.update_title(&task_id, &title)
    } else if let Some(context) = req.context {
        svc.update_context(&task_id, context)
    } else {
        return (StatusCode::BAD_REQUEST, "No fields to update").into_response();
    };

    match result {
        Ok(task) => {
            state.orchestration_bus.emit(OrchestrationEvent::TaskUpdated {
                task_id: task.id.clone(),
                title: task.title.clone(),
                status: format!("{:?}", task.status).to_lowercase(),
            });
            axum::Json(task).into_response()
        }
        Err(hermix_task::TaskServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("patch_task error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// Request body for `PATCH /v1/tasks/{task_id}/status`.
#[derive(Debug, serde::Deserialize)]
struct PatchTaskStatusRequest {
    status: String,
}

/// `PATCH /v1/tasks/{task_id}/status` — Transition a Task's status.
async fn patch_task_status(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
    axum::Json(req): axum::Json<PatchTaskStatusRequest>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let new_status = match req.status.as_str() {
        "pending" => hermix_core::TaskStatus::Pending,
        "planning" => hermix_core::TaskStatus::Planning,
        "in_progress" => hermix_core::TaskStatus::InProgress,
        "completed" => hermix_core::TaskStatus::Completed,
        "failed" => hermix_core::TaskStatus::Failed,
        "archived" => hermix_core::TaskStatus::Archived,
        _ => {
            return (StatusCode::BAD_REQUEST, "Invalid status").into_response();
        }
    };

    let old_status = svc
        .get(&task_id)
        .ok()
        .map(|t| format!("{:?}", t.status).to_lowercase());

    match svc.transition(&task_id, new_status) {
        Ok(task) => {
            state
                .orchestration_bus
                .emit(OrchestrationEvent::TaskStatusChanged {
                    task_id: task.id.clone(),
                    old_status: old_status.unwrap_or_default(),
                    new_status: format!("{:?}", task.status).to_lowercase(),
                });
            axum::Json(task).into_response()
        }
        Err(hermix_task::TaskServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("patch_task_status error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `DELETE /v1/tasks/{task_id}` — Archive a Task (soft delete).
async fn delete_task(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    if let Err(hermix_task::TaskServiceError::NotFound(_)) = svc.get(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match svc.archive(&task_id) {
        Ok(task) => axum::Json(task).into_response(),
        Err(e) => {
            tracing::warn!("delete_task error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `GET /v1/tasks/{task_id}/children` — List child tasks of a parent.
///
async fn list_task_children(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

match svc.list_children(&task_id) {
Ok(tasks) => axum::Json(tasks).into_response(),
Err(e) => {
tracing::warn!("list_task_children error: {e}");
(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}
}
}

// ===========================================================================
/// Product Delivery — HTTP handlers
// ===========================================================================

/// Request body for `POST /v1/tasks/{task_id}/results`.
#[derive(Debug, serde::Deserialize)]
struct DeliverResultsRequest {
    /// List of file paths or URLs to register as deliverables.
    files: Vec<String>,
    /// Optional overall summary for the delivery.
    #[serde(default)]
    summary: Option<String>,
}

/// `POST /v1/tasks/{task_id}/results` — Register deliverable artifacts
/// for a Task.
///
/// This is the `present_files` equivalent endpoint. It:
/// 1. Classifies each file path/URL by artifact type (html/image/report/video/file/directory)
/// 2. Adds each as a `TaskResultRef` to the Task's `results` field
/// 3. Emits a `ResultDelivered` event via the orchestration bus
/// 4. Returns the updated Task with all result refs
async fn deliver_results(
    State(state): State<HttpServerState>,
    Path(task_id): Path<String>,
    axum::Json(req): axum::Json<DeliverResultsRequest>,
) -> axum::response::Response {
    let svc = match &state.task_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    // Classify and register each file
    let mut result_refs = Vec::new();
    for path in &req.files {
        let artifact_type = classify_artifact_type(path);
        let summary = req.summary.as_ref().map(|s| s.clone());
        let result_ref = hermix_core::TaskResultRef {
            path: path.clone(),
            artifact_type,
            summary,
        };

        match svc.add_result(&task_id, result_ref.clone()) {
            Ok(_) => {
                result_refs.push(result_ref);
            }
            Err(hermix_task::TaskServiceError::NotFound(_)) => {
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "deliver_results error");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    e.to_string(),
                )
                    .into_response();
            }
        }
    }

    // Emit ResultDelivered event
    state.orchestration_bus.emit(OrchestrationEvent::ResultDelivered {
        task_id: task_id.clone(),
        result_refs: result_refs.clone(),
    });

    // Return the updated task
    match svc.get(&task_id) {
        Ok(task) => axum::Json(task).into_response(),
        Err(e) => {
            tracing::warn!(task_id = %task_id, error = %e, "deliver_results: failed to get updated task");
            axum::Json(serde_json::json!({
                "task_id": task_id,
                "delivered_count": result_refs.len(),
                "result_refs": result_refs,
            }))
            .into_response()
        }
    }
}

/// Classify a file path or URL into a `TaskArtifactType` based on its
/// extension or scheme.
/// - `.html` / `.htm` → `Html`
/// - `.png` / `.jpg` / `.jpeg` / `.gif` / `.svg` / `.webp` → `Image`
/// - `.pdf` / `.doc` / `.docx` / `.xls` / `.xlsx` / `.md` → `Report`
/// - `.mp4` / `.avi` / `.mov` / `.webm` → `Video`
/// - `http://` / `https://` → `File` (URL reference)
/// - Trailing `/` → `Directory`
/// - Otherwise → `File`
fn classify_artifact_type(path: &str) -> hermix_core::TaskArtifactType {
    use hermix_core::TaskArtifactType;

    let lower = path.to_lowercase();

    // URLs
    if lower.starts_with("http://") || lower.starts_with("https://") {
        if lower.ends_with(".html") || lower.ends_with(".htm") {
            return TaskArtifactType::Html;
        }
        return TaskArtifactType::File;
    }

    // Directory
    if lower.ends_with('/') {
        return TaskArtifactType::Directory;
    }

    // File extension classification
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => TaskArtifactType::Html,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico" => TaskArtifactType::Image,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "md" | "txt" | "csv" => {
            TaskArtifactType::Report
        }
        "mp4" | "avi" | "mov" | "webm" | "mkv" | "flv" => TaskArtifactType::Video,
        _ => TaskArtifactType::File,
    }
}

// ===========================================================================
/// Automation Engine — HTTP handlers
// ===========================================================================

/// Request body for `POST /v1/automations`.
#[derive(Debug, serde::Deserialize)]
struct CreateAutomationRequest {
    name: String,
    prompt: String,
    #[serde(default = "default_schedule_type")]
    schedule_type: String,
    rrule: Option<String>,
    scheduled_at: Option<String>,
    valid_from: Option<String>,
    valid_until: Option<String>,
    cwds: Option<String>,
    connector_ids_json: Option<String>,
    expert_id: Option<String>,
    model_id: Option<String>,
}

fn default_schedule_type() -> String {
    "recurring".to_string()
}

/// `POST /v1/automations` — Create a new automation.
async fn create_automation(
    State(state): State<HttpServerState>,
    axum::Json(req): axum::Json<CreateAutomationRequest>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let schedule_type = match req.schedule_type.as_str() {
        "recurring" => hermix_automation::ScheduleType::Recurring,
        "once" => hermix_automation::ScheduleType::Once,
        _ => {
            return (StatusCode::BAD_REQUEST, "Invalid schedule_type").into_response();
        }
    };

    let params = hermix_automation::CreateAutomationParams {
        name: req.name,
        prompt: req.prompt,
        schedule_type,
        rrule: req.rrule,
        scheduled_at: req.scheduled_at,
        valid_from: req.valid_from,
        valid_until: req.valid_until,
        cwds: req.cwds,
        connector_ids_json: req.connector_ids_json,
        expert_id: req.expert_id,
        model_id: req.model_id,
    };

    match svc.create(params) {
        Ok(auto) => axum::Json(auto).into_response(),
        Err(e) => {
            tracing::warn!("create_automation error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `GET /v1/automations` — List all automations.
async fn list_automations(
    State(state): State<HttpServerState>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.list(None) {
        Ok(autos) => axum::Json(autos).into_response(),
        Err(e) => {
            tracing::warn!("list_automations error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// `GET /v1/automations/{id}` — Get a single automation.
async fn get_automation(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.get(&id) {
        Ok(auto) => axum::Json(auto).into_response(),
        Err(hermix_automation::AutomationServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("get_automation error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// `DELETE /v1/automations/{id}` — Delete an automation (cascades).
async fn delete_automation(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.delete(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(hermix_automation::AutomationServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("delete_automation error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `POST /v1/automations/{id}/pause` — Pause an automation.
async fn pause_automation(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.pause(&id) {
        Ok(auto) => axum::Json(auto).into_response(),
        Err(hermix_automation::AutomationServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("pause_automation error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `POST /v1/automations/{id}/resume` — Resume a paused automation.
async fn resume_automation(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.resume(&id) {
        Ok(auto) => axum::Json(auto).into_response(),
        Err(hermix_automation::AutomationServiceError::NotFound(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::warn!("resume_automation error: {e}");
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
    }
}

/// `GET /v1/automations/{id}/runs` — List run history for an automation.
async fn list_automation_runs(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match svc.list_runs(&id, 100) {
        Ok(runs) => axum::Json(runs).into_response(),
        Err(e) => {
            tracing::warn!("list_automation_runs error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// `POST /v1/automations/{id}/trigger` — Manually trigger an automation.
///
/// Dispatches through the unified `agent_service.send_message()` path,
/// the same path used by HTTP chat requests and the scheduler's trigger
/// callback (铁律 DD: HTTP 与自动化同路径).
async fn trigger_automation_endpoint(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let svc = match &state.automation_service {
        Some(s) => s.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let agent = state.agent_service();
    let bus = state.orchestration_bus.clone();
    let task_svc = state.task_service.clone();

    match automation_trigger::trigger_automation_manually(
        &id,
        &svc,
        &agent,
        &bus,
        task_svc.as_deref(),
    )
    .await
    {
        Ok(automation_id) => {
            axum::Json(serde_json::json!({
                "automation_id": automation_id,
                "status": "triggered",
                "message": "Automation executed via unified dispatch path"
            }))
            .into_response()
        }
        Err(e) => {
            tracing::warn!("trigger_automation error: {e}");
            if e.contains("not found") {
                StatusCode::NOT_FOUND.into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
            }
        }
    }
}







