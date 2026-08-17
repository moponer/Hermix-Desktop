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
use axum::extract::{Path, Query, State};
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
        .route("/v1/protocol/schema", get(protocol_schema))
        .route("/v1/telemetry/client-event", post(client_telemetry_event))
        .route("/metrics", get(metrics_prometheus))
        .route("/v1/metrics", get(metrics_snapshot))
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/v1/sessions/{session_id}",
            post(rename_session).patch(patch_session).delete(delete_session),
        )
        .route(
            "/v1/sessions/{session_id}/messages",
            get(get_session_messages).post(send_message),
        )
        // archive refs surface for the chat UI.
        // `GET /v1/sessions/{id}/archive-refs` returns the per-session
        // JSONL index written by `archive_context_section`. Empty
        // Vec when the session has never been archived.
        .route(
            "/v1/sessions/{session_id}/archive-refs",
            get(get_session_archive_refs),
        )
        //T2-A : moved from /api/sessions/* (dashboard router).
        // The frontend now uses /v1/sessions/{id}/undo and .../restore and .../search.
        .route(
            "/v1/sessions/{session_id}/undo",
            post(undo_session),
        )
        .route(
            "/v1/sessions/{session_id}/restore",
            post(restore_session),
        )
        .route("/v1/sessions/search", get(search_sessions))
        .route("/v1/sessions/{session_id}/lineage", get(get_session_lineage))
        // Approval response REST fallback — when WebSocket delivery
        // fails (Tauri WebView2 can lose buffered messages on connection
        // reset), the frontend sends the approval via HTTP as a backup.
        .route(
            "/v1/sessions/{session_id}/approval",
            post(approve_via_rest),
        )
        // todo (task list) HTTP endpoints. The LLM
        // goes through the `todo` tool over WS; the desktop TodoPanel
        // hits these endpoints for hydration (GET) and manual
        // checkbox toggles (PATCH). See `list_todos` / `patch_todo`.
        .route("/v1/todos", get(list_todos))
        .route(
            "/v1/todos/{todo_id}",
            patch(patch_todo).delete(delete_todo),
        )
        .route("/v1/commands", post(exec_command))
        .route("/v1/ws/{session_id}", get(ws::ws_upgrade))
        .route("/v1/ws-stream/{session_id}", get(ws::ws_stream_upgrade))
        .route("/v1/ws-groupchat/{room_id}", get(ws::ws_groupchat_upgrade))
        .route("/v1/llm/warmup", post(warm_up_provider_handler))
        // Expert catalog (driven by the desktop client's bundled built-ins).
        .route("/v1/experts", get(list_experts).post(register_expert))
        .route("/v1/experts/{id}", get(get_expert))
        // TP1: dynamic tool discovery — the desktop client
        // calls this once at startup and uses the result as the single
        // source of truth for `overrides.tools`. Adding a new tool to
        // the `ToolRegistry` automatically surfaces it in the chat UI
        // and the LLM's tool list, with no front-end code change.
        .route("/v1/tools", get(list_available_tools))
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

#[derive(Serialize)]
struct ExpertListResponse {
    experts: Vec<hermix_skills::ExpertEntry>,
}

async fn list_experts(State(state): State<HttpServerState>) -> Json<ExpertListResponse> {
    Json(ExpertListResponse {
        experts: state.expert_registry.list(),
    })
}

// ---------------------------------------------------------------------------
// TP1: dynamic tool discovery
// ---------------------------------------------------------------------------
// Returns every tool currently registered in the `ToolRegistry` so the
// desktop client can decide what to expose to the LLM without any
// hard-coded tool names on the front-end. Adding a tool to the registry
// (e.g. `tool_registry.register(...)` at startup) makes it appear here
// automatically.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDescriptor {
    name: String,
    toolset: String,
    description: String,
    emoji: String,
    is_async: bool,
    is_enabled: bool,
    env_deps: Vec<String>,
    max_result_size_chars: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolListResponse {
    tools: Vec<ToolDescriptor>,
}

async fn list_available_tools(
    State(state): State<HttpServerState>,
) -> Json<ToolListResponse> {
    let tools = state
        .tool_registry
        .list_tools()
        .into_iter()
        .map(|t| ToolDescriptor {
            name: t.name,
            toolset: t.toolset,
            description: t.description,
            emoji: t.emoji,
            is_async: t.is_async,
            is_enabled: t.is_enabled,
            env_deps: t.env_deps,
            max_result_size_chars: t.max_result_size_chars,
        })
        .collect();
    Json(ToolListResponse { tools })
}

#[derive(Serialize)]
struct ExpertResponse {
    expert: hermix_skills::ExpertEntry,
}

async fn get_expert(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<ExpertResponse>, (StatusCode, String)> {
    match state.expert_registry.get(&id) {
        Some(expert) => Ok(Json(ExpertResponse { expert })),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("expert '{id}' is not registered"),
        )),
    }
}

#[derive(Deserialize)]
struct RegisterExpertRequest {
    experts: Vec<hermix_skills::ExpertEntry>,
}

#[derive(Serialize)]
struct RegisterExpertResponse {
    inserted: usize,
    total: usize,
}

async fn register_expert(
    State(state): State<HttpServerState>,
    Json(req): Json<RegisterExpertRequest>,
) -> Json<RegisterExpertResponse> {
    let inserted = state.expert_registry.register_many(req.experts);
    let total = state.expert_registry.len();
    Json(RegisterExpertResponse { inserted, total })
}

async fn metrics_prometheus() -> impl IntoResponse {
(
[(
header::CONTENT_TYPE,
"text/plain; version=0.0.4; charset=utf-8",
)],
hermix_telemetry::prometheus_text(),
)
}

/// ( / ) JSON snapshot of local metrics
/// for dashboard display. Returns counters, latency percentiles,
/// and buffer status.
async fn metrics_snapshot(
    State(state): State<HttpServerState>,
) -> Json<crate::metrics::MetricsSnapshot> {
    Json(state.metrics.snapshot())
}



async fn protocol_schema() -> impl IntoResponse {
    Json(serde_json::json!({
        "version": 1,
        "ws_envelope": hermix_transport::ws_schema_json()
    }))
}

async fn client_telemetry_event(
    Json(event): Json<ClientTelemetryEvent>,
) -> Result<Json<serde_json::Value>, HttpError> {
    match event.level.as_str() {
        "error" => tracing::error!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
        "warn" => tracing::warn!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
        _ => tracing::info!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_sessions(State(state): State<HttpServerState>) -> Json<ListSessionsResponse> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = match open_sessions_db_for_read(&db_path) {
        Ok(conn) => conn,
        Err(_) => {
            return Json(ListSessionsResponse {
                sessions: vec![],
                total: 0,
            });
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT id, title, created_at, updated_at, message_count, model, workspace, expert_id \
         FROM sessions WHERE id NOT LIKE 'groupchat-%' ORDER BY updated_at DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => {
            return Json(ListSessionsResponse {
                sessions: vec![],
                total: 0,
            });
        }
    };
    let sessions: Vec<SessionSummary> = stmt
        .query_map([], |row| {
            Ok(SessionSummary {
                id: row.get::<_, String>(0)?,
                title: row
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| "New chat".to_string()),
                created_at: row.get::<_, String>(2)?,
                updated_at: row.get::<_, String>(3)?,
                message_count: row.get::<_, i64>(4).unwrap_or(0) as usize,
                model: row.get::<_, Option<String>>(5).ok().flatten(),
                workspace: row.get::<_, Option<String>>(6).ok().flatten(),
                expert_id: row.get::<_, Option<String>>(7).ok().flatten(),
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    Json(ListSessionsResponse {
        total: sessions.len(),
        sessions,
    })
}

async fn create_session(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionSummary>, HttpError> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let title = if req.title.trim().is_empty() {
        "New chat".to_string()
    } else {
        req.title
    };
    state
        .session_persistence
        .persist_session(
            &session_id,
            &[] as &[Message],
            state.config.model.as_deref(),
            Some(HTTP_PLATFORM),
            Some(&title),
            None,
            None,
            // Bind workspace at creation time so a session reload
            // (after desktop restart) restores the workspace category
            // the user picked in the welcome UI. Without this the
            // welcome-state draft workspace only ever lived in
            // `hermix.session.<id>.workspace` localStorage and
            // vanished on restart — Sidebar bucketed the row under
            //"未分组" (bug ). `req.project` is the legacy
            // field name carried over from the createSession wire
            // payload — the desktop client sends the welcome
            // draftWorkspace here on first message.
            req.project.as_deref(),
            None,
        )
        .map_err(HttpError::from)?;
    Ok(Json(SessionSummary {
        id: session_id,
        title,
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
        model: state.config.model.as_deref().map(|s| s.to_string()),
        workspace: req.project.clone(),
        expert_id: None,
    }))
}

async fn rename_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<RenameSessionRequest>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let messages = state
        .session_persistence
        .load_session(&session_id)
        .map_err(HttpError::from)?;
    state
        .session_persistence
        .persist_session(
            &session_id,
                    &messages,
                    // model: None — preserve the session's existing model
                    // binding (a per-session override or the global default).
                    // Passing `config.model` here would clobber a per-session
                    // override on every rename, a regression once the
                    // ON CONFLICT branch in `save_session_atomic` updates
                    // `model` (COALESCE keeps the stored value when we pass
                    // None).
                    None,
            Some(HTTP_PLATFORM),
            Some(req.title.trim()),
            None,
            None,
            // Rename must not clobber the workspace/expert binding the
            // user picked earlier — pass None so save_session_atomic
            // COALESCE leaves the existing columns alone.
            None,
            None,
        )
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// PATCH /v1/sessions/{id}
/// Update session metadata (workspace, expert_id, model).
/// Fields missing from the request body are left unchanged.
async fn patch_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = open_sessions_db_for_read(&db_path).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    let sid = session_id;
    // Build SET clauses dynamically based on what's in the request.
    // A key present with `null` means "clear this field".
    if let Some(v) = req.get("workspace") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET workspace = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET workspace = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    if let Some(v) = req.get("expert_id") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET expert_id = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET expert_id = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    if let Some(v) = req.get("model") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET model = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET model = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Todo (task list) HTTP endpoints
// ---------------------------------------------------------------------------
// The `todo` tool is the canonical mutation path (LLM-driven). These
// endpoints exist so the front-end can (a) hydrate the TodoPanel on
// initial mount via `GET /v1/todos`, and (b) let the user manually
// flip a task to `done` via `PATCH /v1/todos/{id}`. Both endpoints
// dispatch through the same `TodoHandler` so the broadcast path
// (`todo_event_tx`) fires on every change, keeping the WS stream and
// the HTTP loop consistent.
// Why not call FileTodoBackend directly? Two reasons:
// * The broadcast fires only when TodoHandler.execute() runs.
// Bypassing the handler would skip the WS forwarder and leave
// the desktop panel stale.
// * It keeps the schema (parameter names, status enums) in one
// place — the LLM tool definition.
// Trade-off: each `GET` re-enters `ToolHandler::execute`, which
// parses the LLM-style action envelope. For N items this is O(N)
// JSON parse, which is fine (todo lists are <100 items).

async fn list_todos(
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // Commit D — accept optional `?session_id=...` so the TodoPanel
    // doesn't fetch every session's tasks when a single tab is open.
    // Without `session_id` we fall back to the legacy "list all"
    // behaviour so the dashboard's own panels keep working.
    let session_id = params.get("session_id").cloned();
    let raw = match session_id {
        Some(sid) => state
            .tool_registry
            .dispatch(
                "todo",
                serde_json::json!({ "action": "list", "session_id": sid }),
            ),
        None => state
            .tool_registry
            .dispatch("todo", serde_json::json!({ "action": "list" })),
    };
    // dispatch() returns the JSON envelope as a String. The todo
    // tool's list backend returns `{"todos":[...],"total":N}` so
    // forward it as-is; if it's not valid JSON we surface the raw
    // string under a `raw` key for debugging.
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("todo backend returned non-JSON: {e}: {raw}"),
    })?;
    Ok(Json(parsed))
}

async fn patch_todo(
    Path(id): Path<String>,
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // The TodoPanel only ever flips a task to `completed` /
    // `pending`; everything else (title / priority / dependencies)
    // is LLM-driven and goes through the tool directly. Accept
    // any of the canonical fields and forward verbatim so we don't
    // surprise the user when their clicks somehow round-trip a
    // back-end update.
    let mut params_obj = serde_json::json!({
        "action": "update",
        "id": id,
    });
    if let Some(s) = req.get("status").and_then(|v| v.as_str()) {
        params_obj["status"] = serde_json::Value::String(s.to_string());
    }
    if let Some(t) = req.get("title").and_then(|v| v.as_str()) {
        params_obj["title"] = serde_json::Value::String(t.to_string());
    }
    if let Some(p) = req.get("priority").and_then(|v| v.as_str()) {
        params_obj["priority"] = serde_json::Value::String(p.to_string());
    }

    // Commit D — `?session_id=...` so the TodoPanel's checkbox toggle
    // can be attributed to the right chat. Without it the row would
    // be persisted with `session_id = None` (orphan) and the WS
    // snapshot would not include it on the next reconnect.
    // We don't restore the previous session after dispatch: the
    // TodoItem has already been written with the correct session_id
    // (Commit A) and a concurrent agent-loop turn will re-stamp its
    // own session before the next mutation. The registry's stamper
    // is a "last writer wins" cell by design.
    let session_id = params.get("session_id").cloned();
    let _ = state
        .tool_registry
        .set_active_session_for_tool("todo", session_id);

    let raw = state.tool_registry.dispatch("todo", params_obj);

    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        serde_json::json!({ "ok": true, "raw": raw })
    });
    Ok(Json(parsed))
}

async fn delete_todo(
    Path(id): Path<String>,
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // Dispatches through `TodoHandler::execute` so the broadcast path
    // (`todo_event_tx`) fires and the WS forwarder drops the row from
    // every connected TodoPanel — keeping the HTTP loop and the WS
    // stream consistent (same rationale as `patch_todo`).
    // Commit D — same `?session_id=...` rationale as `patch_todo`:
    // thread the session through to the persisted TodoItem so the
    // delete doesn't leave an orphan row in `todos.json`.
    let session_id = params.get("session_id").cloned();
    let _ = state
        .tool_registry
        .set_active_session_for_tool("todo", session_id);

    let raw = state.tool_registry.dispatch(
        "todo",
        serde_json::json!({ "action": "delete", "id": id }),
    );

    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        serde_json::json!({ "ok": true, "raw": raw })
    });
    Ok(Json(parsed))
}

/// Open `sessions.db` for **read-only-ish** server-side access.
/// Every server route that touches `sessions.db` MUST go through this
/// helper rather than calling `Connection::open` directly. Without it,
/// `journal_mode = WAL` only applies to the writer side
/// (`session_persistence::open_with_wal`), and the server opener
/// inherits the legacy `journal_mode = delete` default. That mismatch
/// surfaces `SQLITE_CORRUPT` / "database disk image is malformed" the
/// moment the writer was killed mid-transaction and left a stale
/// `-shm` page table — which happens on every Tauri sidecar SIGKILL
/// shutdown.
/// Best-effort on PRAGMA failures: if WAL mode is rejected (read-only
/// file, exclusive lock held, etc.) we log a warning and continue —
/// Detect the canonical SQLite "database disk image is malformed"
/// diagnostic. extension : the original WAL fix
/// closed the `journal_mode` mismatch, but FTS5 virtual tables can
/// also surface this error when their shadow pages are corrupted.
/// A rebuild of `messages_fts` + `messages_fts_trigram` resolves it,
/// so we treat the error as recoverable rather than fatal.
fn is_malformed_error(err: &rusqlite::Error) -> bool {
    let msg = err.to_string();
    msg.contains("database disk image is malformed")
        || msg.contains("database disk image")
        || msg.contains("malformed")
}

/// Rebuild both FTS5 indexes attached to `messages`. Idempotent —
/// safe to call from a retry path. If the rebuild itself fails the
/// caller should give up and return 500 to the user rather than
/// silently swallow.
/// Both indexes are present from migration 2 onward (the schema
/// migration creates `messages_fts` for CJK bigram search and
/// `messages_fts_trigram` for ASCII substring). On older DBs where
/// trigram is missing, we treat the rebuild as best-effort and
/// continue — failing to rebuild an absent table is not a blocker.
fn rebuild_messages_fts(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('rebuild');")
        .map_err(|e| format!("messages_fts rebuild failed: {e}"))?;
    // trigram may not exist on pre-migration-2 DBs — try and warn,
    // don't fail the whole delete for a missing optional index.
    if let Err(e) = conn.execute_batch(
        "INSERT INTO messages_fts_trigram(messages_fts_trigram) VALUES('rebuild');",
    ) {
        tracing::warn!(
            "rebuild_messages_fts: trigram rebuild skipped ({e});              the trigram index will be lazy-rebuilt on next INSERT"
        );
    }
    Ok(())
}

/// `busy_timeout` alone still resolves the silent-drop / lock-contention
/// case.
fn open_sessions_db_for_read(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(|e| e.to_string())?;
    if let Err(e) = conn.execute_batch("PRAGMA journal_mode = WAL;") {
        tracing::warn!(
            "open_sessions_db_for_read: WAL PRAGMA failed ({e}); \
             continuing with default journaling"
        );
    }
    // Passive checkpoint so we don't read the WAL header from a page
    // the writer hasn't flushed yet. PASSIVE never blocks the writer.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(conn)
}

async fn delete_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = open_sessions_db_for_read(&db_path).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;

    //extension : try the cascade-delete; if it
    // fails with "database disk image is malformed" — typically a
    // corrupted `messages_fts` shadow page — rebuild both FTS5
    // indexes and retry once. If the rebuild itself fails or the
    // retry still errors, fall through to the original 500 surface
    // so the user isn't silently told the delete succeeded.
    // No labelled block needed: the retry path returns directly
    // via `return Ok(Json(...))`, and the happy path falls through
    // to the closing `}` of the for-loop naturally. Both branches
    // exit the function on completion, so there's nothing to break
    // out of.
    let tx = conn.unchecked_transaction().map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;

    // after the DB rows are gone, also remove the on-disk
    // session artifacts so they don't linger as orphaned data. There
    // are TWO separate trees:
    // 1. `<home>/sessions/<sid>/`        — archive-index.jsonl
    // (written via `hermix_home`, NOT data_dir)
    // 2. `<home>/data/sessions/<sid>.md` — session log
    // (written via `data_dir`, flat file)
    // The sidebar list reads from the DB only, so the session won't
    // resurrect — but leaving these behind is an orphaned-data leak.
    // Best-effort: log + ignore on failure; never fail the delete
    // because of leftover files (the DB row is already gone).
    //( / ): typed as a non-capturing `fn`
    // pointer so it can be `Copy`-captured into `spawn_blocking`
    // closures at both call sites without being consumed.
    let cleanup_session_dir: fn(&std::path::Path, &str) = |home, session_id| {
        let data_dir = hermix_config::paths::data_dir_in(home);

        // 1. Archive tree: <hermix_home>/sessions/<sid>/ (holds
        // archive-index.jsonl + per-turn markdown sidecars).
        let archive_dir = home.join("sessions").join(&session_id);
        if archive_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&archive_dir) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: archive dir removal failed (DB row already deleted)"
                );
            }
        }

        // 2. Session log file: <data>/sessions/<sid>.md
        let sessions_dir = data_dir.join("sessions");
        let log_file = sessions_dir.join(format!("{session_id}.md"));
        if log_file.exists() {
            if let Err(e) = std::fs::remove_file(&log_file) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: session log file removal failed (DB row already deleted)"
                );
            }
        }
        // 2b. Legacy per-turn sidecars: any `<stamp>-<sid>.md` left in
        // sessions_dir (normally migrated to sessions_dir/orphans/
        // on the next save, but a deleted session never gets one).
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(&format!("-{session_id}.md"))
                    && name != format!("{session_id}.md")
                {
                    if let Err(e) = std::fs::remove_file(entry.path()) {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "delete_session: legacy session sidecar removal failed"
                        );
                    }
                }
            }
        }

        // 3. RL trajectory: <data>/trajectories/<sid>.json (written by
        // SessionPersistence::save_trajectory on every completed turn).
        let traj_file = data_dir.join("trajectories").join(format!("{session_id}.json"));
        if traj_file.exists() {
            if let Err(e) = std::fs::remove_file(&traj_file) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: trajectory file removal failed (DB row already deleted)"
                );
            }
        }
    };

    // purge the deleted session's recall memory from
    // memory.db so a removed conversation leaves zero vector/embedding
    // residue. memory.db is WAL + owned by the same process as
    // sessions.db, so a second `Connection::open` (mirroring
    // `open_sessions_db_for_read`) is the established, safe pattern.
    // Every session-scoped store is covered:
    // - frames / facts            (tagged `session:{id}`, JSON `tags`)
    // - recall_audit / session_snapshots (explicit `session_id` col)
    // - frames_fts / facts_fts    (FTS shadow tables; must stay in
    // sync with frames/facts or future searches corrupt)
    // Best-effort: any failure is logged + ignored; the session delete
    // itself must never fail because the memory purge hiccupped.
    //: same `fn`-pointer pattern as `cleanup_session_dir`.
    let purge_session_memory: fn(&std::path::Path, &str) = |hermix_home, session_id| {
        let mem_path =
            hermix_config::paths::data_dir_in(hermix_home)
                .join("memory.db");
        let Ok(conn) = rusqlite::Connection::open(&mem_path) else {
            tracing::warn!(
                session_id = %session_id,
                "delete_session: memory.db missing, skipping recall-memory purge"
            );
            return;
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL;");
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");

        let like = format!("%\"session:{}\"%", session_id);
        let mut total: usize = 0;

        // FTS shadow tables first — their id subquery still reads the
        // live frames/facts rows.
        let _ = conn.execute(
            "DELETE FROM frames_fts WHERE id IN (SELECT id FROM frames WHERE tags LIKE ?1)",
            rusqlite::params![like],
        );
        let _ = conn.execute(
            "DELETE FROM facts_fts WHERE id IN (SELECT id FROM facts WHERE tags LIKE ?1)",
            rusqlite::params![like],
        );
        if let Ok(n) = conn.execute("DELETE FROM frames WHERE tags LIKE ?1", rusqlite::params![like]) {
            total += n as usize;
        }
        if let Ok(n) = conn.execute("DELETE FROM facts WHERE tags LIKE ?1", rusqlite::params![like]) {
            total += n as usize;
        }
        // Explicit session_id columns (older DBs without these tables
        // simply no-op the DELETE — swallowed by `if let Ok`).
        if let Ok(n) = conn.execute(
            "DELETE FROM recall_audit WHERE session_id = ?1",
            rusqlite::params![session_id],
        ) {
            total += n as usize;
        }
        if let Ok(n) = conn.execute(
            "DELETE FROM session_snapshots WHERE session_id = ?1",
            rusqlite::params![session_id],
        ) {
            total += n as usize;
        }
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");

        if total > 0 {
            tracing::info!(
                session_id = %session_id,
                purged_rows = total,
                "delete_session: purged session recall memory from memory.db"
            );
        }
    };

    // Cascade-delete all child tables that reference sessions(id)
    let tables = [
        "messages",
        "compression_archives",
        "compression_summary_history",
        "token_usage",
    ];
    {
        for table in &tables {
            if let Err(e) = tx.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?1"),
                rusqlite::params![session_id],
            ) {
                if !is_malformed_error(&e) {
                    return Err(HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: e.to_string(),
                    });
                }
                // Roll back the partial cascade, rebuild FTS, retry once.
                let _ = tx.rollback();
                tracing::warn!(
                    table = %table,
                    error = %e,
                    "delete_session: cascade DELETE hit 'malformed'; rebuilding messages_fts and retrying"
                );
                if let Err(rb_err) = rebuild_messages_fts(&conn) {
                    return Err(HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: format!(
                            "delete_session: cascade failed ({e}); FTS rebuild also failed ({rb_err})"
                        ),
                    });
                }
                let tx2 = conn.unchecked_transaction().map_err(|e| HttpError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: e.to_string(),
                })?;
                for retry_table in &tables {
                    tx2.execute(
                        &format!("DELETE FROM {retry_table} WHERE session_id = ?1"),
                        rusqlite::params![session_id],
                    )
                    .map_err(|e2| HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: format!(
                            "delete_session: retry after FTS rebuild still failed on `{retry_table}`: {e2}"
                        ),
                    })?;
                }
                let deleted = tx2
                    .execute(
                        "DELETE FROM sessions WHERE id = ?1",
                        rusqlite::params![session_id],
                    )
                    .map_err(|e| HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: e.to_string(),
                    })?;
                if deleted == 0 {
                    let _ = tx2.rollback();
                    return Err(HttpError {
                        status: StatusCode::NOT_FOUND,
                        message: format!("session '{session_id}' not found"),
                    });
                }
                tx2.commit().map_err(|e| HttpError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: e.to_string(),
                })?;
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                // same success-path logging/cleanup as the
                // happy path below (see cleanup_session_dir + tracing::info).
                //: side effects run fire-and-forget on the
                // blocking pool so the HTTP response returns
                // immediately and the async worker stays free.
                let home = state.hermix_home.clone();
                let sid = session_id.clone();
                tokio::task::spawn_blocking(move || {
                    cleanup_session_dir(&home, &sid);
                    purge_session_memory(&home, &sid);
                });
                tracing::info!(
                    session_id = %session_id,
                    "delete_session: deleted session + cascade child rows (after FTS rebuild)"
                );
                return Ok(Json(serde_json::json!({
                    "ok": true,
                    "fts_rebuilt": true,
                })));
            }
        }
        // Happy path — fall through to the original
        // DELETE-from-sessions / 404 / commit / wal-truncate block.
        // Replace `tx` semantics: re-bind for the rest of the fn.
        // We use a labelled break above + fall-through here, so the
        // original block can still execute against the same `tx`.
        // (Rust doesn't allow reassigning `tx`, so we use the
        // labelled block to short-circuit the retry path and let
        // execution continue with `tx` already in scope below.)
    }
    let deleted = tx
        .execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;
    if deleted == 0 {
        // Caller asked to delete a session that no longer exists.
        // Either it was already removed (idempotent retry) or the
        // path is wrong. Roll back the child-table DELETEs we just
        // ran (they might be no-ops if no rows matched, but the
        // rollback is the safe default) and report 404 so the
        // front-end can stop showing the spinner.
        let _ = tx.rollback();
        return Err(HttpError {
            status: StatusCode::NOT_FOUND,
            message: format!("session '{session_id}' not found"),
        });
    }

    tx.commit().map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    // Truncate the WAL after commit so the on-disk `sessions.db` is
    // fully consistent — next opener (or a future restart) won't have
    // to replay a half-checkpointed WAL.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");

    // surface the deletion in backend logs. Previously the
    // happy path was completely silent, so operators/owners had no way
    // to confirm a delete actually happened.
    //: side effects run fire-and-forget on the blocking pool
    // — the DB row is already gone; fs cleanup + memory purge are
    // best-effort and must not block the HTTP response or the async
    // runtime worker (same class of fix as Bug #3).
    let home = state.hermix_home.clone();
    let sid = session_id.clone();
    tokio::task::spawn_blocking(move || {
        cleanup_session_dir(&home, &sid);
        purge_session_memory(&home, &sid);
    });
    tracing::info!(
        session_id = %session_id,
        "delete_session: deleted session + cascade child rows (messages/compression_archives/compression_summary_history/token_usage)"
    );

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /v1/sessions/{id}/archive-refs` — return every [`ArchiveRef`]
/// the agent loop has written for this session.
/// backing storage is a JSONL file at
/// `<hermix_home>/sessions/<sid>/archive-index.jsonl`. We don't go
/// through `AgentService` / `AgentLoop::last_archive_refs()` because
/// that field only holds the **most recent** archive run — for the
/// chat banner we want the full history of archived nodes so the
/// user can re-open any of them.
/// Returns `200 { refs: [] }` when:
/// - the session has never triggered a compression round (common),
/// - the index file is missing / empty,
/// - the session id is malformed (we still don't 404 because the
/// state of "session exists but never archived" is a valid one).
/// All errors during file IO are swallowed (logged) and surfaced as
/// an empty list — the chat UI must never crash just because the
/// archive sidecar had a bad write.
async fn get_session_archive_refs(
    Path(session_id): Path<String>,
) -> Json<ArchiveRefsResponse> {
    let hermix_home = hermix_config::paths::hermix_home();
    let refs = hermix_agent::read_session_index(&hermix_home, &session_id);
    Json(ArchiveRefsResponse { refs })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveRefsResponse {
    refs: Vec<ArchiveRef>,
}

async fn get_session_messages(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<SessionMessagesResponse>, HttpError> {
    // use the *timestamped* loader so each entry
    // carries the persisted `messages.created_at` and the desktop
    // client can render the real relative time on session reload
    // (e.g. "3 天前" instead of "刚刚"). The legacy
    // `load_session_with_approvals` was paired with `timestamp: now`
    // below, which masked every message's true age.
    let entries_with_ts = state
        .session_persistence
        .load_session_with_approvals_and_timestamps(&session_id)
        .map_err(HttpError::from)?;
    let entries: Vec<hermix_agent::SessionEntry> =
        entries_with_ts.iter().map(|(e, _)| e.clone()).collect();
    // Pull the timestamps out in lockstep with the entries below.
    let timestamps: Vec<Option<String>> =
        entries_with_ts.into_iter().map(|(_, ts)| ts).collect();
    let now = Utc::now().to_rfc3339();

    // Build tool_call_id → arguments map from all assistant messages so
    // we can reconstruct tool arguments for role: "tool" rows on reload.
    // The DB stores tool_call_id on tool messages and tool_calls[{id,
    // function.arguments}] on assistant messages — linking them at
    // serve time avoids adding an `arguments` column to the DB and a
    // new field to the Rust `Message` struct.
    let mut args_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for entry in &entries {
        if let hermix_agent::SessionEntry::Message(m) = entry {
            if let Some(ref tcs) = m.tool_calls {
                for tc in tcs {
                    args_map.insert(tc.id.clone(), tc.function.arguments.clone());
                }
            }
        }
    }

    // (Plan C): load archived messages from compression
    // child sessions so the user can still see what was said before
    // compression. Children are ordered by creation time (oldest
    // first), so the archived turns appear in chronological order
    // before the main session's active messages.
    let child_ids = state
        .session_persistence
        .find_children(&session_id)
        .unwrap_or_default();
    let mut archived_entries_with_ts: Vec<(String, Vec<(hermix_agent::SessionEntry, Option<String>)>)> = Vec::new();
    for child_id in &child_ids {
        let child_ets = state
            .session_persistence
            .load_session_with_approvals_and_timestamps(child_id)
            .unwrap_or_default();
        // Also build args_map from archived entries so tool argument
        // reconstruction works for archived tool messages.
        for (entry, _) in &child_ets {
            if let hermix_agent::SessionEntry::Message(m) = entry {
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        args_map.insert(tc.id.clone(), tc.function.arguments.clone());
                    }
                }
            }
        }
        archived_entries_with_ts.push((child_id.clone(), child_ets));
    }

    let mut idx = 0usize;
    let mut all_messages: Vec<ProtocolMessage> = Vec::new();
    // FIX: buffer compression-child messages here instead of
    // extending `all_messages` immediately. They are prepended *after*
    // the main session messages are built so we can de-duplicate child
    // copies against the parent's (possibly reactivated) active rows.
    let mut child_all: Vec<ProtocolMessage> = Vec::new();

    // Archived messages from compression child sessions (oldest first).
    // These carry `compressed: Some(true)` so the frontend can render
    // a subtle "已压缩" marker on them.
    for (child_id, child_ets) in archived_entries_with_ts {
        let child_msgs: Vec<ProtocolMessage> = child_ets
            .into_iter()
            .filter(|(e, _ts)| !matches!(e, hermix_agent::SessionEntry::Message(m) if matches!(m.role, MessageRole::System)))
            .filter_map(|(entry, ts)| {
                match entry {
                    hermix_agent::SessionEntry::Message(m) => {
                        let id = format!("{}-{}", child_id, idx);
                        idx += 1;
                        let tool_calls = m.tool_calls.clone();
                        let tool_name = m.name.clone();
                        let execution_timeline = m.execution_timeline.clone();
                        let tool_arguments = if matches!(m.role, MessageRole::Tool) {
                            m.tool_call_id.as_ref().and_then(|id| args_map.get(id)).cloned()
                        } else {
                            None
                        };
                        let timestamp = ts.clone().unwrap_or_else(|| now.clone());
                        Some(ProtocolMessage {
                            id,
                            role: match m.role {
                                MessageRole::User => hermix_transport::Role::User,
                                MessageRole::Assistant => hermix_transport::Role::Assistant,
                                MessageRole::System => hermix_transport::Role::System,
                                MessageRole::Tool => hermix_transport::Role::Tool,
                            },
                            content: m.content.unwrap_or_default(),
                            timestamp,
                            model: None,
                            tool_calls,
                            tool_name,
                            tool_arguments,
                            extra: None,
                            reasoning_content: m.reasoning_content.clone(),
                            execution_timeline,
                            created_at: ts,
                            compressed: Some(true),
                        })
                    }
                    hermix_agent::SessionEntry::Approval(record)
                    | hermix_agent::SessionEntry::ApprovalOrphan(record) => {
                        let id = format!("{}-approval-{}", child_id, idx);
                        idx += 1;
                        Some(ProtocolMessage {
                            id,
                            role: hermix_transport::Role::Approval,
                            content: String::new(),
                            timestamp: ts.clone().unwrap_or_else(|| now.clone()),
                            model: None,
                            tool_calls: None,
                            tool_name: None,
                            tool_arguments: None,
                            extra: Some(serde_json::to_value(&record).unwrap_or(serde_json::Value::Null)),
                            reasoning_content: None,
                            execution_timeline: None,
                            created_at: ts,
                            compressed: Some(true),
                        })
                    }
                }
            })
            .collect();
        child_all.extend(child_msgs);
    }

    // Main session messages (not compressed).
    let main_msgs: Vec<ProtocolMessage> = entries
        .into_iter()
        .zip(timestamps.into_iter())
        // Defense-in-depth: the SQL filter in
        // `load_session_with_approvals` already excludes role='system'
        // rows. This match-arm is here so even if a future caller
        // accidentally loads a different entry set (e.g. a test
        // fixture) the system prompt never reaches the wire.
        .filter(|(e, _ts)| !matches!(e, hermix_agent::SessionEntry::Message(m) if matches!(m.role, MessageRole::System)))
        .map(|(entry, ts)| match entry {
            hermix_agent::SessionEntry::Message(m) => {
                let id = format!("{}-{}", session_id, idx);
                idx += 1;
                // Forward `tool_calls` (assistant rows) and `name`
                // (`role: "tool"` rows) so the desktop client can
                // rebuild the same inline tool cards the user saw
                // mid-stream on session reload. Without this,
                // `ProtocolMessage` only carries
                // `id/role/content/timestamp` and the front-end is
                // forced to fall back to a generic "tool" label.
                let tool_calls = m.tool_calls.clone();
                let tool_name = m.name.clone();
                // forward the persisted execution
                // timeline so session reloads can rebuild the same
                // inline tool cards the user saw mid-stream
                // (correct status, arguments, stdout). Without this,
                // every tool call after a session reload rendered
                // as a stale "运行中" pill because no event marked
                // them as completed.
                let execution_timeline = m.execution_timeline.clone();
                // reconstruct tool arguments from
                // the linked assistant message's tool_calls. Without
                // this, reloaded tool messages have no `args` and
                // the desktop client falls back to "调用工具" (generic).
                let tool_arguments = if matches!(m.role, MessageRole::Tool) {
                    m.tool_call_id
                        .as_ref()
                        .and_then(|id| args_map.get(id))
                        .cloned()
                } else {
                    None
                };
                // use the persisted `created_at`
                // (carried in via the parallel query) as the wire
                // timestamp so the desktop client shows the real
                // relative time on reload. Fall back to `now` only
                // if the timestamp query somehow missed this row
                // (shouldn't happen — same filter on both sides).
                let timestamp = ts.clone().unwrap_or_else(|| now.clone());
                ProtocolMessage {
                    id,
                    role: match m.role {
                        MessageRole::User => hermix_transport::Role::User,
                        MessageRole::Assistant => hermix_transport::Role::Assistant,
                        MessageRole::System => hermix_transport::Role::System,
                        MessageRole::Tool => hermix_transport::Role::Tool,
                    },
                    // (Bug B): persist already runs through
                    // `ToolCallSanitizer` in `stream_collect` and the
                    // ThinkBlockParser strips `<think>...</think>` from
                    // visible content at write time, so what we read
                    // back here is already clean. The earlier
                    // "defence-in-depth" feed+flush on the read path
                    // was a *streaming state machine* applied to a
                    // whole-string input — when the persisted content
                    // happened to contain a lone `<think>` (no
                    // `</think>`), the sanitizer would enter
                    // `InsideThink`, hold the rest of the string, and
                    // `flush()` would discard it (held text is not
                    // re-emitted on flush). Net effect: the user
                    // reloaded the session and the assistant bubble
                    // was blank. Sanitization on the read path is
                    // fundamentally the wrong shape — content is a
                    // complete blob, not a stream — so we just trust
                    // what was persisted.
                    content: m.content.unwrap_or_default(),
                    timestamp,
                    model: None,
                    tool_calls,
                    tool_name,
                    tool_arguments,
                    extra: None,
                    reasoning_content: m.reasoning_content.clone(),
                    execution_timeline,
                    created_at: ts,
                    compressed: None,
                }
            }
            hermix_agent::SessionEntry::Approval(record)
            | hermix_agent::SessionEntry::ApprovalOrphan(record) => {
                let id = format!("{}-approval-{}", session_id, idx);
                idx += 1;
                // Embed the full ApprovalRecord as JSON for the UI to
                // consume directly. The frontend `ApprovalMessage` type
                // mirrors this shape (requestId / tool / args /
                // riskLevel / decision / decidedAt / linkedAssistantId).
                ProtocolMessage {
                    id,
                    role: hermix_transport::Role::Approval,
                    content: String::new(),
                    timestamp: ts.clone().unwrap_or_else(|| now.clone()),
                    model: None,
                    // Approval rows don't carry tool-calls; the
                    // tool name and risk for the request live in
                    // `extra` (mirrors `ApprovalRecord`). The two
                    // new fields are `None` here.
                    tool_calls: None,
                    tool_name: None,
                    tool_arguments: None,
                    extra: Some(serde_json::to_value(&record).unwrap_or(serde_json::Value::Null)),
                    reasoning_content: None,
                    // Approval rows are not associated with any
                    // assistant turn's tool calls, so no timeline.
                    execution_timeline: None,
                    created_at: ts,
                    compressed: None,
                }
            }
        })
        .collect();
    // FIX (restore+compression duplicate): a compression
    // child session stores a *copy* of the folded-away turns (written
    // with active=1 into the child). At compression time the parent's
    // own copy of those turns is deactivated, so normally only the
    // child copy is shown. But after `/restore` (restore_last_undone)
    // reactivates the parent's turns, BOTH the parent's reactivated
    // rows AND the child's copy would be returned — every archived
    // turn rendered twice in the UI. Drop a child message when an
    // identical (role+content) message already exists among the
    // parent's active messages. Because the child copy is a faithful
    // byte-for-byte copy of the original turn content, this signature
    // match is exact; legitimate distinct turns are never collapsed.
    let role_sig = |r: &hermix_transport::Role| -> &'static str {
        match r {
            hermix_transport::Role::User => "user",
            hermix_transport::Role::Assistant => "assistant",
            hermix_transport::Role::System => "system",
            hermix_transport::Role::Tool => "tool",
            hermix_transport::Role::Approval => "approval",
        }
    };
    let main_sigs: std::collections::HashSet<String> = main_msgs
        .iter()
        .map(|m| format!("{}|{}", role_sig(&m.role), m.content))
        .collect();
    let child_filtered: Vec<ProtocolMessage> = child_all
        .into_iter()
        .filter(|m| !main_sigs.contains(&format!("{}|{}", role_sig(&m.role), m.content)))
        .collect();
    // Preserve chronological order: archived (child) first, then the
    // parent's active messages.
    all_messages.extend(child_filtered);
    all_messages.extend(main_msgs);

    let context_usage = state.session_persistence.get_context_usage(&session_id);
    Ok(Json(SessionMessagesResponse {
        session_id,
        messages: all_messages,
        context_usage,
    }))
}

pub(crate) async fn send_message(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, HttpError> {
    hermix_telemetry::record_http_request();
    let trace_id = hermix_transport::new_trace_id();

    // Build AgentOverrides from request
    let overrides = hermix_core::traits::AgentOverrides {
        model: req.model.clone(),
        provider: req.provider.clone(),
        personality: req.personality.clone(),
        system_prompt: req.system_prompt.clone(),
        tools: req.tools.clone(),
        workspace: req.workspace.clone(),
        permission: parse_permission_level(req.permission.as_deref()),
        images: convert_wire_images(&req.images),
        image: None,
        image_mime_type: None,
        skills: req.skills.clone(),
        memory_dir: None,
        approval_requester: None,
        on_event: None,
        pre_approval_checker: None,
        on_prefetch_intent: None,
        // type-B: non-streaming HTTP path doesn't own a WS
        // socket, so there is no soft-shutdown signal to wake any
        // approval `rx.await`. Leave None — it falls back to plain
        // `rx.await` (which here has no race because this path
        // doesn't abort a task).
        shutdown: None,
    };

    // Call agent service
    let reply = state
        .agent_service
        .send_message(&session_id, &req.text, overrides)
        .await
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;

    Ok(Json(SendMessageResponse {
        session_id,
        reply: reply.text,
        message_count: reply.message_count,
        trace_id,
    }))
}

//T2-A : moved from `dashboard/sessions.rs`. The dashboard
// router was deleted and these endpoints now share the same `/v1/sessions/*`
// namespace as the rest of the session API.

// ── /undo (soft-delete) ─────────────────────────────────────────────

/// POST /v1/sessions/{session_id}/undo?count=N
/// Soft-delete the most recent `count` user-to-assistant turns.
/// Default count is 1 if the query string is absent.
async fn undo_session(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let count: usize = params
        .get("count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let n = state
        .session_persistence
        .mark_recent_turns_inactive(&session_id, count)
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "deactivated": n,
        "turns_requested": count,
    })))
}

/// POST /v1/sessions/{session_id}/restore
/// Restore the most recently deactivated block of messages. Returns
/// the number of messages that were reactivated.
async fn restore_session(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let n = state
        .session_persistence
        .restore_last_undone(&session_id)
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "restored": n,
    })))
}

/// POST /v1/sessions/{session_id}/approval
/// REST fallback for approval responses when WebSocket delivery fails.
/// The frontend sends the approval via HTTP when `ws.send()` returns true
/// but the server doesn't receive the message (Tauri WebView2 WebSocket
/// can lose buffered messages on connection reset).
/// Request body: `{ "request_id": "...", "decision": "approved" | "denied" }`
async fn approve_via_rest(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let request_id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| HttpError {
            status: StatusCode::BAD_REQUEST,
            message: "missing request_id".into(),
        })?;

    let decision = body
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("approved");

    let choice = hermix_transport::ApprovalChoice::from_decision_str(decision);
    let choice = match choice {
        Some(c) => c,
        None => hermix_transport::ApprovalChoice::Deny,
    };

    let approved = choice.is_approved();
    let outcome = if approved {
        hermix_core::ApprovalOutcome::Approved
    } else {
        hermix_core::ApprovalOutcome::DeniedByUser
    };

    let matched = state
        .approval_registry
        .resolve(&session_id, request_id, outcome);

    tracing::info!(
        session_id = %session_id,
        request_id = %request_id,
        decision = %decision,
        matched,
        "REST approval: {}",
        if matched { "hit" } else { "miss — no pending entry" }
    );

    // Persist the decision to the session timeline
    let decision_str = match choice {
        hermix_transport::ApprovalChoice::Once => "once",
        hermix_transport::ApprovalChoice::Session => "session",
        hermix_transport::ApprovalChoice::Always => "always",
        hermix_transport::ApprovalChoice::Deny => "deny",
    };
    let _ = state
        .session_persistence
        .resolve_approval_message(&session_id, request_id, decision_str);

    //Record side effects for session/always choices. P6 :
    // resource-scope approvals (P6 direction 1, `risk: "resource"`)
    // carry path/command-prefix grants — handled by the dedicated
    // resource-side-effect path that persists to GatewayConfig. The
    // existing command-only path remains as a fallback for the
    // Dangerous-tier terminal approvals that don't carry a path.
    // Only record on the **first** successful resolve. Duplicate
    // responses (matched=false from the resolve call above) would
    // otherwise re-grant the same path / re-add the same allowlist
    // regex. `grant_path` / `grant_command` are idempotent, but
    // `UserAllowlist::add_literal` and `session_bypass` push are
    // not — so the matched gate is the cheapest place to dedupe.
    if matched
        && matches!(
            choice,
            hermix_transport::ApprovalChoice::Session
                | hermix_transport::ApprovalChoice::Always
        )
    {
        // Try the resource path first — for non-resource approvals
        // (legacy terminal danger), `lookup_pending_resource` returns
        // `(None, None)` and the function returns early without
        // touching the profile. The regular command-side-effect then
        // runs as the fallback.
        record_resource_approval_side_effect(&state, &session_id, request_id, choice).await;
        if let Ok(Some(cmd)) =
            ws::lookup_pending_command(&state, &session_id, request_id).await
        {
            record_approval_side_effect(&state, &session_id, &cmd, choice);
        }
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "request_id": request_id,
        "matched": matched,
        "decision": decision_str,
    })))
}

/// `GET /v1/sessions/{session_id}/lineage` — return the parent→ancestor
/// chain (upward) and immediate children (downward) for session lineage
/// browsing in the frontend tree view.
/// Response: `{ ancestors: [SessionNode], children: [SessionNode], current: SessionNode }`
async fn get_session_lineage(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    #[derive(Serialize)]
    struct SessionNode {
        id: String,
        title: String,
        has_compression: bool,
    }

    let sp = &state.session_persistence;

    // Walk up the parent chain.
    let mut ancestors: Vec<SessionNode> = Vec::new();
    let mut cursor = sp.get_parent(&session_id)?;
    while let Some(ref pid) = cursor {
        if pid.is_empty() {
            break;
        }
        if let Some((title, _)) = sp.get_session_title(pid)? {
            ancestors.push(SessionNode {
                id: pid.clone(),
                title: title.clone(),
                has_compression: sp.load_compressed_summary(pid).is_some(),
            });
        } else {
            ancestors.push(SessionNode {
                id: pid.clone(),
                title: String::new(),
                has_compression: false,
            });
        }
        cursor = sp.get_parent(pid)?;
    }

    // Immediate children.
    let child_ids = sp.find_children(&session_id)?;
    let mut children: Vec<SessionNode> = Vec::new();
    for cid in &child_ids {
        let (title, _) = sp.get_session_title(cid)
            .unwrap_or_else(|_| None)
            .unwrap_or((String::new(), None));
        children.push(SessionNode {
            id: cid.clone(),
            title,
            has_compression: sp.load_compressed_summary(cid).is_some(),
        });
    }

    // Current node.
    let (cur_title, _) = sp.get_session_title(&session_id)
        .unwrap_or_else(|_| None)
        .unwrap_or((String::new(), None));
    let current = SessionNode {
        id: session_id.clone(),
        title: cur_title,
        has_compression: sp.load_compressed_summary(&session_id).is_some(),
    };

    Ok(Json(serde_json::json!({
        "ancestors": ancestors,
        "children": children,
        "current": current,
    })))
}

/// GET /v1/sessions/search?q=...
async fn search_sessions(
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<SessionSearchResponse> {
    let q = params.get("q").cloned().unwrap_or_default();
    let limit: u32 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let results = match open_sessions_db_for_read(&db_path) {
        Ok(conn) => query_search_with_conn(&conn, &q, limit).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    Json(SessionSearchResponse { results })
}

/// FTS5 search across the messages table. Returns snippets ordered by
/// rank. Best-effort — on any read failure returns an empty vec so the
/// dashboard search box never 500s.
/// FTS5 search using an already-opened connection (so callers that
/// already hold a handle — e.g. `search_sessions` after opening via
/// `open_sessions_db_for_read` — don't pay for a second open).
fn query_search_with_conn(
    conn: &rusqlite::Connection,
    query: &str,
    limit: u32,
) -> Result<Vec<SessionSearchResult>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT m.session_id, snippet(messages_fts, 0, '>>>', '<<<', '...', 48) as snip,
                    m.role, s.platform, s.model, s.created_at
             FROM messages_fts AS mf
             JOIN messages AS m ON m.id = mf.rowid
             JOIN sessions AS s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let results = stmt
        .query_map(rusqlite::params![query, limit], |row| {
            let session_id: String = row.get(0)?;
            let snippet: String = row.get(1)?;
            let role: Option<String> = row.get(2)?;
            let source: Option<String> = row.get(3)?;
            let model: Option<String> = row.get(4)?;
            let created_at: Option<String> = row.get(5)?;
            let session_started = created_at.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|d| d.timestamp() as f64)
                    .ok()
            });

            Ok(SessionSearchResult {
                session_id,
                snippet,
                role,
                source,
                model,
                session_started,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

async fn exec_command(
    State(state): State<HttpServerState>,
    Json(req): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, HttpError> {
    hermix_telemetry::record_http_request();
    let trimmed = req.command.trim();
    if trimmed.is_empty() {
        return Ok(Json(CommandResponse {
            accepted: false,
            output: "empty command".to_string(),
        }));
    }

    let cmd = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    let session_id = req
        .session_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());

    // For now, we'll just send the command as a regular message
    // In a real implementation, we'd need to handle slash commands specially
    let overrides = hermix_core::traits::AgentOverrides::default();
    let reply = state
        .agent_service
        .send_message(&session_id, &cmd, overrides)
        .await
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;

    Ok(Json(CommandResponse {
        accepted: true,
        output: reply.text,
    }))
}

/// POST /v1/llm/warmup — UI 切换模型时触发对应 provider 预热。
/// 立即返回，实际 1-token 探测在 agent service 内 spawn 执行。
async fn warm_up_provider_handler(
    State(state): State<HttpServerState>,
    Json(req): Json<WarmUpProviderRequest>,
) -> StatusCode {
    if !req.model.is_empty() && req.model != "unknown" {
        state.agent_service.warm_up_provider(&req.model);
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct WarmUpProviderRequest {
    model: String,
}

pub(crate) fn record_approval_side_effect(
    state: &HttpServerState,
    session_id: &str,
    command: &str,
    choice: hermix_transport::ApprovalChoice,
) {
    match choice {
        hermix_transport::ApprovalChoice::Session => {
            // Build a `^command$` regex (escaped, like
            // `UserAllowlist::add_literal`) and append to the
            // session's bypass list. Duplicates are tolerated —
            // the `pre_approval_checker` short-circuits on the
            // first match anyway.
            let pattern = format!("^{}$", regex::escape(command));
            match Regex::new(&pattern) {
                Ok(re) => {
                    if let Ok(mut map) = state.session_bypass.lock() {
                        map.entry(session_id.to_string())
                            .or_default()
                            .push(re);
                        tracing::info!(
                            session = %session_id,
                            command = %command,
                            "session-bypass: added command"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        command = %command,
                        error = %e,
                        "session-bypass: failed to compile pattern; side effect skipped"
                    );
                }
            }
        }
        hermix_transport::ApprovalChoice::Always => {
            // Append to the persistent `UserAllowlist`. We
            // lazily initialize the `Option<UserAllowlist>` on
            // the first `always` if the server started up with
            // no allowlist (a fresh install picking "Always
            // Allow" for the first time).
            if let Ok(mut guard) = state.user_allowlist.lock() {
                if guard.is_none() {
                    *guard = Some(UserAllowlist::new());
                }
                if let Some(al) = guard.as_mut() {
                    if let Err(e) = al.add_literal(command) {
                        tracing::warn!(
                            command = %command,
                            error = %e,
                            "always-allow: failed to add entry; side effect skipped"
                        );
                        return;
                    }
                } else {
                    return;
                }
            } else {
                return;
            }
            // Persist the updated allowlist. Failure here is
            // logged but not propagated to the user — the
            // in-memory state is already correct and a failed
            // save just means the next restart loses this one
            // entry (the user can re-pick `always` next time).
            let path = hermix_config::paths::approval_allowlist_path();
            if let Ok(guard) = state.user_allowlist.lock() {
                if let Some(al) = guard.as_ref() {
                    if let Err(e) = al.save(&path) {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "always-allow: failed to persist allowlist; \
                             in-memory state is correct but the change \
                             won't survive a restart"
                        );
                    } else {
                        tracing::info!(
                            command = %command,
                            path = %path.display(),
                            "always-allow: persisted to disk"
                        );
                    }
                }
            }
        }
        _ => {
            // `Once` / `Deny` don't need a side effect — the
            // registry's resolve is the whole story for them.
        }
    }
}

/// P6 : resource-scope side effect. Used when the
/// approval request carries `risk: "resource"` (P6 direction 1),
/// meaning the user is authorising access to a specific path or
/// command prefix the agent requested. `Always` persists the
/// grant into `GatewayConfig.permission_profile` and writes it
/// back to `config.yaml`; `Session` keeps the grant in-memory only
/// (the existing `session_bypass` mechanism is for terminal
/// commands and doesn't carry paths). `Once` / `Deny` are no-ops.
pub(crate) async fn record_resource_approval_side_effect(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
    choice: hermix_transport::ApprovalChoice,
) {
    let (path, command) = match ws::lookup_pending_resource(state, session_id, request_id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                request_id = %request_id,
                error = %e,
                "resource-approval: failed to look up pending row; side effect skipped"
            );
            return;
        }
    };
    if path.is_none() && command.is_none() {
        // Non-resource approval (e.g. the existing Dangerous-tier
        // terminal approval) — fall through to the regular
        // command-based side effect path.
        return;
    }
    match choice {
        hermix_transport::ApprovalChoice::Always => {
            // 1. Pull a fresh GatewayConfig snapshot (the user may
            // have edited it via Settings since startup).
            let mut cfg: hermix_config::GatewayConfig = state
                .agent_service
                .current_config_value()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(|| state.config.as_ref().clone());
            // 2. Apply grants. Mirrors the
            // `PendingGrant::from_tool_call` heuristic in
            // hermix-core so server- and client-side agree on
            // what "grant this resource" means.
            if let Some(p) = &path {
                cfg.permission_profile.grant_path(p);
            }
            if let Some(c) = &command {
                cfg.permission_profile.grant_command(c);
            }
            // 3. Persist to config.yaml so the grant survives
            // server restart. Failure is logged but does not
            // abort the request — the in-memory state is
            // already pushed to the agent service below.
            let config_path = state.hermix_home.join("config.yaml");
            if let Err(e) = hermix_config::save_config_yaml(&config_path, &cfg) {
                tracing::warn!(
                    error = %e,
                    path = %config_path.display(),
                    "permission_profile: failed to persist config.yaml; \
                     in-memory state is correct but the change won't survive a restart"
                );
            } else {
                tracing::info!(
                    path = ?path,
                    command = ?command,
                    config = %config_path.display(),
                    "permission_profile: persisted Always-grant to config.yaml"
                );
            }
            // 4. Push the updated config to the live agent service
            // so the next chat's `build_agent_config` picks up
            // the new profile without a server restart.
            match serde_json::to_value(&cfg) {
                Ok(value) => state.agent_service.set_config(value),
                Err(e) => tracing::warn!(
                    error = %e,
                    "permission_profile: failed to serialise GatewayConfig for set_config"
                ),
            }
        }
        hermix_transport::ApprovalChoice::Session => {
            // In-memory only: live-push to the agent service.
            // Persisting session-scoped grants to disk would muddy
            // the "session" semantics — restart should clear them.
            let mut cfg: hermix_config::GatewayConfig = state
                .agent_service
                .current_config_value()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(|| state.config.as_ref().clone());
            if let Some(p) = &path {
                cfg.permission_profile.grant_path(p);
            }
            if let Some(c) = &command {
                cfg.permission_profile.grant_command(c);
            }
            if let Ok(value) = serde_json::to_value(&cfg) {
                state.agent_service.set_config(value);
            }
            tracing::info!(
                session_id = %session_id,
                path = ?path,
                command = ?command,
                "permission_profile: in-memory Session-grant applied (no persistence)"
            );
        }
        _ => {
            // `Once` / `Deny` are no-ops at the profile layer; the
            // agent already has the immediate grant via the
            // on_pre_tool_approved hook for this turn only.
        }
    }
}

pub(crate) fn redact_sensitive(input: &str) -> String {
    let mut out = input.to_string();
    let patterns = [
        ("sk-", "sk-***"),
        ("ghp_", "ghp_***"),
        ("xoxb-", "xoxb-***"),
    ];
    for (needle, replacement) in patterns {
        if out.contains(needle) {
            out = out.replace(needle, replacement);
        }
    }
    out
}

/// turn a raw `AgentError` from the agent loop into a
/// user-facing `(code, message)` pair.
/// Each classified category prepends a short actionable Chinese prefix
/// (e.g. "AI 服务过载:") followed by the **original provider error
/// message**, so the user can see what actually happened (which model,
/// which provider, what the API returned) instead of a generic
/// one-size-fits-all banner. Everything else falls back to
/// `e.to_string()` so we never lose diagnostic detail.
pub(crate) fn user_facing_agent_error(e: &str) -> (String, String) {
    let s = e.to_string();
    let lower = s.to_lowercase();

    /// Extract the human-readable message from a raw error string.
    /// Provider responses are often nested JSON like:
    /// `API error 529 <unknown status code>: {"type":"error","error":{"message":"负载较高..."}}`
    /// This function strips the `API error NNN` prefix and extracts
    /// the inner `message` field if present, falling back to the
    /// stripped string otherwise.
    fn extract_provider_message(raw: &str) -> String {
        // Strip repeated "LLM API error: API error:" wrappers
        let stripped = raw
            .replace("LLM API error: ", "")
            .replace("LLM API error: ", "")
            .replace("API error ", "");
        // Try to extract the provider's own message from JSON
        // e.g. `529 <unknown status code>: {"type":"error","error":{"message":"负载较高..."}}`
        if let Some(json_start) = stripped.find('{') {
            let json_part = &stripped[json_start..];
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_part) {
                if let Some(msg) = parsed
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return msg.to_string();
                }
                if let Some(msg) = parsed.get("message").and_then(|m| m.as_str()) {
                    return msg.to_string();
                }
            }
        }
        // Not JSON or no message field — return the stripped version
        stripped.trim().to_string()
    }

    let provider_msg = extract_provider_message(&s);

    if lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("用量上限")
        || lower.contains("token plan")
        || lower.contains("quota")
    {
        (
            "quota_exhausted".to_string(),
            format!("用量已达上限: {provider_msg}"),
        )
    } else if lower.contains("529")
        || lower.contains("overloaded")
        || lower.contains("负载较高")
        || lower.contains("server is overloaded")
        || lower.contains("service unavailable")
        || lower.contains("503")
    {
        (
            "provider_overloaded".to_string(),
            format!("AI 服务过载: {provider_msg}"),
        )
    } else if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("invalid api key")
    {
        (
            "auth_error".to_string(),
            format!("API 密钥认证失败: {provider_msg}"),
        )
    } else {
        ("agent_error".to_string(), s)
    }
}

// ---------------------------------------------------------------------------
// Visible-text sanitizer for raw tool-call markers in LLM output.
// The redactor itself lives in `hermix_core::tool_call_sanitizer`
// (see `hermix_core::ToolCallSanitizer`). It is a *stateful*
// state-machine: opening and closing XML tags may arrive in different
// streaming chunks, and a per-chunk regex pass (the previous
// implementation) would leak any block whose opener and closer were
// split across a chunk boundary. The new implementation holds the
// in-between chunks until the matching close arrives.
// ## Why this lives in two places
// The agent loop (`hermix_agent::agent_loop::apply_stream_collect_chunk`)
// runs one copy so the *persisted* assistant message is clean on
// reload. The WS layer (`hermix_server::lib` on_chunk) runs a second
// copy so the *live* stream never shows leaked XML. The two passes
// are idempotent (the second pass is a no-op once the text is
// already clean), so the doubling is intentional belt-and-suspenders
// and not a bug.
// ## What it does NOT do
// It does NOT try to *execute* the embedded tool call. That would
// require either (a) telling the agent loop to use a different
// text-based parser as the authoritative source, or (b) pre-parsing
// the streamed text and synthesising `tool_start` / `tool_complete`
// events out-of-band. Both are bigger surface-area changes; the
// narrow "don't show the XML to the user" fix is enough to stop the
// visible leak today.
// The original regression tests for this leak have been moved to
// `hermix_core::tool_call_sanitizer::tests`.

/// Map the `permission` field of [`SendMessageRequest`] (a free-form string
/// the frontend hands us) to the strongly-typed [`PermissionLevel`]. Unknown
/// values fall back to `Default` so a malformed payload never escalates
/// privileges silently.
fn parse_permission_level(value: Option<&str>) -> Option<PermissionLevel> {
    match value.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("default") | None => Some(PermissionLevel::Default),
        Some("full") => Some(PermissionLevel::Full),
        Some(other) => {
            tracing::warn!(
                permission = %other,
                "unknown permission level in SendMessageRequest; falling back to Default"
            );
            Some(PermissionLevel::Default)
        }
    }
}

/// Convert wire-format images (raw bytes) to agent-format images (base64).
/// 多图支持 — client sends `Vec<ImageAttachment>` with binary
/// `bytes`; the LLM API needs `data:<mime>;base64,<...>` URLs, so we
/// encode here.
pub(crate) fn convert_wire_images(
    images: &Option<Vec<hermix_transport::ImageAttachment>>,
) -> Option<Vec<hermix_core::traits::AgentImage>> {
    images.as_ref().map(|imgs| {
        imgs.iter()
            .map(|img| hermix_core::traits::AgentImage {
                base64: base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                mime_type: img.mime_type.clone(),
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------
// L3: Voice bridge — HTTP endpoints for STT + TTS
// ---------------------------------------------------------------------------

/// `POST /v1/voice/transcribe` — accept base64-encoded 16-bit PCM
/// and return transcribed text. Body shape matches
/// `hermix_voice::TranscribeRequest`; response matches
/// `hermix_voice::TranscribeResponse`.
pub(crate) fn extract_llm_probe_url(config: &GatewayConfig) -> Option<(String, Option<String>)> {
    // 1) Try the active model string ("provider:model")
    if let Some(ref model_str) = config.model {
        if let Some((prov, _)) = model_str.split_once(':') {
            if let Some(p) = config.llm_providers.get(prov) {
                if let Some(ref url) = p.base_url {
                    return Some((prov.to_string(), Some(url.clone())));
                }
                // No base_url set, but we can still return provider name so
                // the runtime can infer a default URL from it.
                if p.api_key.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
                    || std::env::var(os_env_var(prov)).ok().map(|s| !s.is_empty()).unwrap_or(false)
                {
                    return Some((prov.to_string(), None));
                }
            }
        }
    }
    // 2) Fall back to the first provider that is "configurable" (api_key OR env
    // var OR explicit base_url). Without an api_key/env, the runtime can't
    // actually authenticate, but returning the provider name + base_url lets
    // the caller at least surface a useful default (and probe it).
    for (name, p) in &config.llm_providers {
        let has_key = p.api_key.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
            || std::env::var(os_env_var(name)).ok().map(|s| !s.is_empty()).unwrap_or(false);
        let has_url = p.base_url.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        if has_key || has_url {
            return Some((name.clone(), p.base_url.clone()));
        }
    }
    None
}

/// Map a provider name to its canonical env var name for key detection.
fn os_env_var(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "qwen" | "dashscope" => "DASHSCOPE_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "kimi" | "moonshot" => "MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "nous" => "NOUS_API_KEY",
        "copilot" => "GITHUB_COPILOT_TOKEN",
        "siliconflow" => "SILICONFLOW_API_KEY",
        "zhipu" => "ZHIPU_API_KEY",
        "volcengine" => "ARK_API_KEY",
        _ => "UNKNOWN_API_KEY",
    }
}

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
        let conn = open_sessions_db_for_read(&db).expect("open");

        // The DELETE itself should trigger the malformed error path
        // and our helper should rebuild the FTS index + retry.
        // (We test the helper directly because the actual
        // `delete_session` async fn is hard to drive from a unit
        // test without spinning up the full axum router.)
        let tx = conn.unchecked_transaction().unwrap();
        let res = tx.execute("DELETE FROM messages WHERE session_id = 'fts-test'", []);
        assert!(res.is_err(), "expected malformed error");
        assert!(is_malformed_error(&res.unwrap_err()));
        let _ = tx.rollback();

        // Rebuild and retry — this is exactly what delete_session
        // does on the recovery path.
        rebuild_messages_fts(&conn).expect("rebuild");
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
        assert!(!is_malformed_error(&err));
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

        let conn = open_sessions_db_for_read(&db).unwrap();
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
        let conn = open_sessions_db_for_read(&db).expect("missing db should auto-create");
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





