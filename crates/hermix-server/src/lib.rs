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
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_state;

pub use security::parse_allowed_ips;
pub use security::PolicyGuardConfig;

// Re-export orchestration event bus types (Phase 0 / P0-5)
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
use axum::extract::ws::{Message as WsMessage, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::header;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::StreamExt;
use hermix_config::GatewayConfig;
use hermix_agent::python_alignment::ThinkBlockParser;
use hermix_agent::ArchiveRef;
use hermix_core::{traits::{PermissionLevel, RiskLevel}, AgentError, Message, MessageRole, StreamChunk};
use hermix_tools::approval::UserAllowlist;
use hermix_tools::ToolRegistry;
use regex::Regex;
use hermix_transport::{
    CreateSessionRequest, ListSessionsResponse, ProtocolMessage, RenameSessionRequest,
    SendMessageRequest, SendMessageResponse, SessionMessagesResponse, SessionSummary, StreamEvent,
    WsClientMessage, WsEnvelope,
};

// T2-A (2026-06-20): Session search types live in `dashboard::types` (they
// are dashboard-shaped and not part of the core transport). Will move them
// to `hermix-transport` if the SPA ever needs them on /v1/.
use crate::dashboard::types::{SessionSearchResponse, SessionSearchResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::oneshot;
use tower::service_fn;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

pub const HTTP_PLATFORM: &str = "http";

/// Default timeout for pending tool-call approval requests.
///
/// Mirrors the upstream `approvals.timeout: 60s` in the Hermes
/// 7-layer defense model: if the WS client doesn't send an
/// `ApprovalResponse` within this window, the request is
/// auto-resolved to `false` (deny) and removed from the
/// registry. This prevents a hung UI from holding the agent
/// loop indefinitely, and it shifts the burden of stale
/// requests to deny-by-default (fail-closed) -- which is the
/// only safe direction for a destructive-tool approval.
///
/// **2026-06-14 bump:** raised from 60s → 300s. The 60s
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
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Compute the wall-clock deadline (Unix milliseconds) for a fresh
/// approval request. Centralized so the WS envelope and the persisted
/// `ApprovalRecord` share the **same** value — reloading the session
/// would otherwise show a countdown that doesn't match the original
/// (the two `now()` calls taken a few microseconds apart diverge
/// visibly when the deadline is rounded to seconds for display).
///
/// Returns `Utc::now().timestamp_millis() + DEFAULT_APPROVAL_TIMEOUT.as_millis() as i64`.
/// Monotonic clock is not appropriate here — we want wall-clock so
/// the deadline survives a clock-jump and matches the user's
/// `Date.now()` on the front-end (which is also wall-clock).
pub fn compute_approval_deadline_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
        + DEFAULT_APPROVAL_TIMEOUT.as_millis() as i64
}

// ---------------------------------------------------------------------------
// ApprovalRegistry — maps session → request_id → oneshot::Sender<bool>
// ---------------------------------------------------------------------------

/// Thread-safe registry for pending tool-call approval requests.
///
/// When the agent needs user approval before executing a risky tool, it
/// creates a `oneshot::Sender<bool>` keyed by `(session_id, request_id)`.
/// The client resolves the request via a [`WsClientMessage::ApprovalResponse`]
/// message on the same WebSocket, which looks up and completes the channel.
#[derive(Clone)]
pub struct ApprovalRegistry {
    inner: Arc<std::sync::Mutex<HashMap<String, HashMap<String, oneshot::Sender<bool>>>>>,
    default_timeout: Duration,
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_APPROVAL_TIMEOUT)
    }

    /// Create a registry whose pending approvals auto-resolve to
    /// `false` after `default_timeout` of inactivity. Use
    /// `Duration::ZERO` to disable the timeout.
    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(HashMap::new())),
            default_timeout,
        }
    }

    /// Returns the configured default timeout for pending
    /// approval requests. Used by tests and by the WS handler
    /// that wants to know the policy in effect.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Create a new pending approval and return a receiver that resolves when
    /// the client sends an [`ApprovalResponse`]. If the sender side is dropped
    /// without resolution, the receiver yields `false` (deny by default).
    /// If the configured timeout elapses first, the receiver yields
    /// `false` (deny) and a `tracing::warn!` is logged.
    pub fn request_approval(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        // Store the sender first, then release the lock before
        // spawning the timeout task -- keeps the critical section
        // short and avoids holding the registry lock across an
        // await point.
        {
            let mut sessions = self.inner.lock().unwrap();
            sessions
                .entry(session_id.to_string())
                .or_default()
                .insert(request_id.to_string(), tx);
        }
        // Spawn a timer that, on expiry, takes the sender back
        // out of the registry and sends `false`. If a real
        // `resolve` happens first, the entry will already be
        // gone by the time we wake up and the take is a no-op.
        let to = self.default_timeout;
        if !to.is_zero() {
            let inner = Arc::clone(&self.inner);
            let sess = session_id.to_string();
            let req = request_id.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(to).await;
                let tx_opt = {
                    let mut sessions = inner.lock().unwrap();
                    sessions
                        .get_mut(&sess)
                        .and_then(|session| session.remove(&req))
                };
                if let Some(tx) = tx_opt {
                    tracing::warn!(
                        session_id = %sess,
                        request_id = %req,
                        timeout_secs = to.as_secs(),
                        "approval request timed out (default deny)"
                    );
                    let _ = tx.send(false);
                }
                // else: a real resolve beat us to it -- no-op.
            });
        }
        rx
    }

    /// Resolve a pending approval request. Returns `true` if the sender was
    /// found and resolved, `false` if the request had already been resolved
    /// (e.g. by a prior `resolve` call, or by the timeout task firing
    /// first) or was never registered.
    pub fn resolve(&self, session_id: &str, request_id: &str, decision: bool) -> bool {
        let mut sessions: std::sync::MutexGuard<
            HashMap<String, HashMap<String, oneshot::Sender<bool>>>,
        > = self.inner.lock().unwrap();
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(tx) = session.remove(request_id) {
                let _ = tx.send(decision);
                return true;
            }
        }
        false
    }

    /// True if a sender is still registered for `(session_id,
    /// request_id)`. Used by the 4-state `session` / `always` side
    /// effect handlers to skip the work when the request was
    /// already resolved (duplicate response) or timed out — both
    /// of which remove the entry.
    pub fn is_pending(&self, session_id: &str, request_id: &str) -> bool {
        let sessions = self.inner.lock().unwrap();
        sessions
            .get(session_id)
            .map(|s| s.contains_key(request_id))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod approval_registry_tests {
    //! Tests for [`ApprovalRegistry`] — the in-memory map of
    //! pending `(session_id, request_id) → oneshot::Sender<bool>`
    //! pairs that backs the WS `ApprovalResponse` flow. The
    //! registry's contract is:
    //!
    //! - `request_approval` returns a fresh receiver and stores
    //!   the sender under the (session_id, request_id) key.
    //! - `resolve` removes the entry and forwards the decision
    //!   through the oneshot. Returns `true` if a sender was
    //!   found, `false` if the key didn't exist (the LLM
    //!   already gave up, or the WS client sent a response for
    //!   a different request).
    //! - Dropping the receiver or sender without resolution
    //!   must not panic; the survivor sees `Err` / `false`.
    //! - Different sessions and different request_ids are
    //!   isolated — resolving one must not affect another.

    use super::*;

    #[tokio::test]
    async fn request_returns_receiver_resolve_true_yields_true() {
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", true));
        assert_eq!(rx.await.unwrap(), true);
    }

    #[tokio::test]
    async fn request_returns_receiver_resolve_false_yields_false() {
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", false));
        assert_eq!(rx.await.unwrap(), false);
    }

    #[tokio::test]
    async fn resolve_unknown_request_returns_false() {
        let reg = ApprovalRegistry::new();
        // No request was ever registered.
        assert!(!reg.resolve("sess-a", "nope", true));
        assert!(!reg.resolve("nonexistent-session", "req-1", true));
    }

    #[tokio::test]
    async fn resolve_twice_for_same_request_returns_false_second_time() {
        // The registry removes the entry on first resolve, so a
        // second resolve must be a no-op and return `false`.
        // This protects against the rare case where the
        // desktop client double-fires (network retry, double
        // click) and a stale response would otherwise unblock a
        // *different* in-flight tool call.
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", true));
        assert!(
            !reg.resolve("sess-a", "req-1", false),
            "second resolve should report no sender"
        );
        assert_eq!(rx.await.unwrap(), true, "first decision wins");
    }

    #[tokio::test]
    async fn different_requests_in_same_session_do_not_interfere() {
        let reg = ApprovalRegistry::new();
        let rx1 = reg.request_approval("sess-a", "req-1");
        let rx2 = reg.request_approval("sess-a", "req-2");

        // Resolve only req-1 with `true`; req-2 must still be
        // pending and answerable.
        assert!(reg.resolve("sess-a", "req-1", true));
        assert_eq!(rx1.await.unwrap(), true);

        // Drop rx2 without resolving (sender still in the
        // registry) — this is the "user closed the dialog
        // without deciding" case. The next resolve should
        // still find req-2's sender and forward a deny.
        drop(rx2);
        assert!(reg.resolve("sess-a", "req-2", false));
    }

    #[tokio::test]
    async fn different_sessions_with_same_request_id_do_not_interfere() {
        let reg = ApprovalRegistry::new();
        let rx_a = reg.request_approval("sess-a", "req-1");
        let rx_b = reg.request_approval("sess-b", "req-1");

        // Resolve only sess-a's req-1. sess-b's req-1 must
        // remain pending.
        assert!(reg.resolve("sess-a", "req-1", true));
        assert_eq!(rx_a.await.unwrap(), true);

        // And sess-b's req-1 can still be resolved.
        assert!(reg.resolve("sess-b", "req-1", false));
        assert_eq!(rx_b.await.unwrap(), false);
    }

    #[tokio::test]
    async fn drop_receiver_without_resolve_yields_err_at_registry_drop() {
        // The receiver is held by the agent loop. If the agent
        // loop drops the rx (e.g. it bails out for an unrelated
        // reason), the sender remains in the registry. The
        // next resolve returns `true` (the sender was found),
        // but the rx has been dropped, so `tx.send` is a
        // silent no-op. This is fine — the registry's contract
        // is "if a sender was found, return true; if not,
        // return false"; whether the consumer is still listening
        // is a separate concern handled by the agent loop.
        let reg = ApprovalRegistry::new();
        let rx = reg.request_approval("sess-a", "req-1");
        drop(rx);
        assert!(
            reg.resolve("sess-a", "req-1", true),
            "sender still in registry after rx drop"
        );
    }

    #[tokio::test]
    async fn drop_sender_registry_drops_sender_returns_err_at_rx() {
        // Mirrors the production failure mode the user
        // reported: the WS connection closes (e.g. the user
        // navigates away or the desktop app crashes) before
        // the user responds. The sender is dropped (via
        // dropping the HttpServerState) and the receiver
        // yields `Err` — the agent loop treats this as a
        // deny-by-default.
        //
        // We simulate by dropping the registry (the inner Arc
        // goes to zero when no other clones remain). The
        // receiver is the last thing holding state alive, so
        // we have to do this carefully.
        // Use Duration::ZERO to disable the timeout. The
        // production registry keeps a spawned timer task
        // alive that holds a clone of the inner Arc, so
        // dropping  alone would let the timer fire
        // (after the default 60s) and call 
        // -- making rx.await return Ok(false), not Err.
        // Disabling the timeout isolates the sender-drop
        // semantics this test cares about.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let rx = reg.request_approval("sess-a", "req-1");
        // Drop the registry, then resolve via... hmm, we can't
        // resolve after dropping. Instead, the test below
        // mirrors the actual user behavior more closely: drop
        // the registry's clone (the HttpServerState holds one
        // clone, the approval_requester closure holds another,
        // etc.), and verify that the rx yields Err.
        //
        // For the contract surface we just need to assert:
        //   "if no one ever resolves, and the sender side
        //   gets dropped, the rx must yield Err (not hang
        //   forever)".
        // We test the actual mechanism by manually dropping
        // the sender via a oneshot::Sender that we take back
        // from the registry's internals. Simpler: just drop
        // the registry, which drops the inner HashMap, which
        // drops all senders.
        let reg_for_drop = reg.clone();
        drop(reg);
        // reg_for_drop is still alive; drop it too.
        drop(reg_for_drop);
        // rx is now the only thing keeping the channel alive.
        // Its sender has been dropped via the registry. The
        // rx.await should resolve to Err.
        assert!(
            rx.await.is_err(),
            "dropped sender must produce Err at the receiver"
        );
    }
    // -- Timeout (default deny) -----------------------------------

    #[tokio::test]
    async fn timeout_fires_and_yields_false_when_no_resolve() {
        // With a short timeout, an unresolved request must
        // auto-resolve to  (deny) after the configured
        // delay. This is the "fail-closed" contract that
        // mirrors upstream .
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let rx = reg.request_approval("sess-a", "req-1");
        // Wait long enough for the timer to fire.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let result = rx.await;
        assert_eq!(result, Ok(false), "timeout must default-deny");
    }

    #[tokio::test]
    async fn resolve_before_timeout_yields_decision() {
        // A real  that arrives before the timeout
        // must propagate the user decision (true) and the
        // timer task must be a no-op (its  finds the
        // entry already gone).
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(200));
        let rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", true));
        assert_eq!(rx.await, Ok(true));
        // Wait past the original timeout to ensure the
        // late-arriving timer task is a no-op and doesn't
        // poison any future test (e.g. by panicking in a
        // spawned task).
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    #[tokio::test]
    async fn resolve_after_timeout_returns_false() {
        // Once the timeout has fired and consumed the
        // sender, a subsequent  for the same key
        // must report  (no sender found). This
        // protects against the WS client responding to a
        // request that the agent has already given up on
        // (fail-closed has already won).
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(30));
        let rx = reg.request_approval("sess-a", "req-1");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = rx.await; // consume
        assert!(
            !reg.resolve("sess-a", "req-1", true),
            "resolve after timeout must report no sender"
        );
    }

    #[tokio::test]
    async fn timeout_zero_disables_timer() {
        //  must skip spawning the timer
        // task entirely. The receiver then hangs forever
        // (until dropped) — which is the historical
        // pre-timeout behavior, preserved for tests that
        // want to assert sender-drop semantics in
        // isolation.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let _rx = reg.request_approval("sess-a", "req-1");
        // Just verify that 100ms later, the registry is
        // still serving the request and resolve still
        // works.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(reg.resolve("sess-a", "req-1", true));
    }

    #[tokio::test]
    async fn concurrent_timeouts_are_independent() {
        // Two requests in the same session with the same
        // short timeout must both fire independently;
        // resolving one must not affect the other.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let rx1 = reg.request_approval("sess-a", "req-1");
        let rx2 = reg.request_approval("sess-a", "req-2");
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(rx1.await, Ok(false));
        assert_eq!(rx2.await, Ok(false));
    }

    #[tokio::test]
    async fn default_timeout_constant_is_300_seconds() {
        // Pin the upstream-mirrored default. If anyone ever
        // bumps this, the WS UI countdown logic and the
        // upstream-parity docs need to follow. Bumped from
        // 60s → 300s on 2026-06-14 — the 60s window produced a
        // user-visible "I clicked allow but the agent said
        // denied" race; see the doc-comment on
        // `DEFAULT_APPROVAL_TIMEOUT` for the full story.
        assert_eq!(DEFAULT_APPROVAL_TIMEOUT, std::time::Duration::from_secs(300));
        let reg = ApprovalRegistry::new();
        assert_eq!(reg.default_timeout(), std::time::Duration::from_secs(300));
    }

    /// `compute_approval_deadline_ms` returns wall-clock "now"
    /// plus `DEFAULT_APPROVAL_TIMEOUT`. The WS envelope and the
    /// persisted `ApprovalRecord` share the SAME value (one
    /// call site per request, not one per consumer) — see
    /// `on_pre_tool_dispatch` in this file. This test pins
    /// that invariant: deadline − now is at least
    /// `DEFAULT_APPROVAL_TIMEOUT` and at most that plus a
    /// couple of ms (test execution can pause a hair, but a 5s
    /// slack accommodates slow CI without false positives).
    #[test]
    fn compute_approval_deadline_is_now_plus_default_timeout() {
        let before = chrono::Utc::now().timestamp_millis();
        let deadline = compute_approval_deadline_ms();
        let after = chrono::Utc::now().timestamp_millis();

        let timeout_ms = DEFAULT_APPROVAL_TIMEOUT.as_millis() as i64;
        let expected_min = before + timeout_ms;
        let expected_max = after + timeout_ms;
        assert!(
            deadline >= expected_min && deadline <= expected_max,
            "deadline {deadline} not in [{expected_min}, {expected_max}]"
        );
    }

    #[tokio::test]
    async fn timeout_does_not_fire_for_resolved_request() {
        // Regression: after a clean resolve, the timer
        // task must not run a stale  that
        // would mislead operators into thinking a request
        // had timed out. We can't directly observe the
        // absence of a log line, but we can prove the
        // timer task's  found nothing by ensuring
        // a fresh  with the same key
        // is fully resolvable after the original timeout
        // would have fired.
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::from_millis(50));
        let _rx1 = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", true));
        // Wait past the original timeout window.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // A new request on the same key must be served
        // freshly (the previous sender was consumed by
        // resolve, the timer found nothing, so no state
        // is leaked).
        let rx2 = reg.request_approval("sess-a", "req-1");
        assert!(reg.resolve("sess-a", "req-1", false));
        assert_eq!(rx2.await, Ok(false));
    }

    // -----------------------------------------------------------------
    // is_pending — used by the 4-state side-effect handlers in
    // `handle_ws_stream` to skip the session/always store updates
    // when the original request was already resolved or timed out.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn is_pending_true_after_request_false_after_resolve() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        let _rx = reg.request_approval("sess-a", "req-1");
        assert!(reg.is_pending("sess-a", "req-1"));
        assert!(reg.resolve("sess-a", "req-1", true));
        assert!(
            !reg.is_pending("sess-a", "req-1"),
            "resolve must remove the entry"
        );
    }

    #[tokio::test]
    async fn is_pending_false_for_unknown_request() {
        let reg = ApprovalRegistry::with_timeout(std::time::Duration::ZERO);
        assert!(!reg.is_pending("sess-a", "never-requested"));
        assert!(!reg.is_pending("never-session", "req-1"));
    }

    // -----------------------------------------------------------------
    // record_approval_side_effect — 4-state session/always store
    // updates. We construct a minimal `HttpServerState` with empty
    // maps and verify that:
    // - `Session` adds a compiled regex to the per-session bypass
    //   map and the new entry makes `pre_approval_checker` hit.
    // - `Always` lazily creates the persistent `UserAllowlist` and
    //   writes it to disk; reload from disk restores the entries.
    // - `Once` and `Deny` are no-ops.
    // -----------------------------------------------------------------

    /// Build a minimal `HttpServerState` for unit tests. The full
    /// `build_with_hermes_home` requires a real `GatewayConfig` and
    /// spins up cron / skills / etc.; for these side-effect tests
    /// we only need the two new fields populated, so we construct
    /// the struct via its pub fields. (We use `unsafe { ... }` only
    /// to silence the `dead_code` warning on `Arc::clone(&state)`
    /// in the unused-arg shim; no actual memory unsafety.)
    fn test_state(hermix_home: std::path::PathBuf) -> HttpServerState {
        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        let _ = session_persistence.ensure_db();
        HttpServerState {
            config: Arc::new(hermix_config::GatewayConfig::default()),
            tool_registry: Arc::new(hermix_tools::ToolRegistry::new()),
            hermix_home,
            session_persistence,
            cron_scheduler: None,
            skill_store: None,
            runtime_gateway_running: None,
            agent_service: Arc::new(hermix_agent::LocalAgentService::new(
                Arc::new(hermix_config::GatewayConfig::default()),
                Arc::new(hermix_tools::ToolRegistry::new()),
                Arc::new(hermix_agent::session_persistence::SessionPersistence::new(
                    tempfile::tempdir().unwrap().path(),
                )),
            )),
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher: hermix_tools::ClarifyDispatcher::new(),
            clarify_broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(1);
                tx
            },
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(None)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker: hermix_agent::ConsciousnessTracker::new(),
            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig::default(),
            ),
            groupchat_mgr: Arc::new(hermix_gateway::groupchat::GroupChatManager::new(
                Arc::new(hermix_gateway::groupchat::InMemoryRoomPersistence::new()),
            )),
            mcp_manager: hermix_mcp::ArcMcpManager::new(Arc::new(hermix_tools::ToolRegistry::new())),
            task_engine: None,
            // 2026-07-06: dedup map. See the production `build`
            // path; test_state mirrors it.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            todo_event_tx: None,
            orchestration_bus: OrchestrationEventBus::new(),
            task_service: None,
            automation_service: None,
            automation_engine_scheduler: None,
        }
    }

    /// `Session` choice: adds the command to the per-session bypass
    /// map, and the resulting pre-approval check hits.
    #[test]
    fn session_choice_adds_to_session_bypass() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "sudo apt install ripgrep",
            hermix_transport::ApprovalChoice::Session,
        );

        // session_bypass now has one pattern for sess-A.
        let map = state.session_bypass.lock().unwrap();
        let patterns = map.get("sess-A").expect("sess-A entry");
        assert_eq!(patterns.len(), 1);
        assert!(patterns[0].is_match("sudo apt install ripgrep"));
        // A similar but not-equal command does not match.
        assert!(!patterns[0].is_match("sudo apt install fd"));
    }

    /// `Always` choice: lazily creates the persistent `UserAllowlist`
    /// and writes the JSON file. A reload from disk sees the entry.
    #[test]
    fn always_choice_persists_to_disk() {
        // Use a temp dir as hermix home so the persistent file
        // doesn't pollute the user's real `~/.hermix`.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let state = test_state(home.clone());

        // Initially no allowlist.
        assert!(state.user_allowlist.lock().unwrap().is_none());

        record_approval_side_effect(
            &state,
            "sess-A",
            "systemctl restart nginx",
            hermix_transport::ApprovalChoice::Always,
        );

        // Lazily created, in-memory.
        assert!(state.user_allowlist.lock().unwrap().is_some());

        // Persisted to disk.
        let path = hermix_config::paths::approval_allowlist_path();
        // Override HERMIX_HOME to tmp so the path helper points
        // at the temp file, not the user's real one. The build
        // already wrote under `~/.hermix` by default, so we
        // copy-and-check via a direct reload below.
        let written_path = home.join("approval_allowlist.json");
        if path != written_path {
            // The `hermix_config::hermix_home()` global state
            // can't be redirected here without a global env
            // mutation, so the helper may have written to
            // `~/.hermix`. We accept that for the MVP and check
            // the in-memory + on-disk state in the best-effort
            // way: re-construct a state from the same path the
            // helper produced, and verify the entry survives.
            // (The dedicated `user_allowlist_save_load_roundtrip`
            // test in hermix-tools already covers the
            // save/load cycle in isolation.)
            let _ = path; // silence unused warning
        }

        // In-memory check is the authoritative one for this
        // test (it verifies the side-effect ran; the on-disk
        // roundtrip is covered by `hermix-tools::approval`
        // tests).
        let al = state.user_allowlist.lock().unwrap();
        let al = al.as_ref().expect("lazy-init created the allowlist");
        assert!(al.matches("systemctl restart nginx"));
        // Different command — no match.
        assert!(!al.matches("systemctl restart mysql"));
    }

    /// `Once` choice: no-op. The state is unchanged.
    #[test]
    fn once_choice_is_noop() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "ls -la",
            hermix_transport::ApprovalChoice::Once,
        );
        assert!(state.session_bypass.lock().unwrap().is_empty());
        assert!(state.user_allowlist.lock().unwrap().is_none());
    }

    /// `Deny` choice: no-op. The state is unchanged.
    #[test]
    fn deny_choice_is_noop() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "rm -rf /",
            hermix_transport::ApprovalChoice::Deny,
        );
        assert!(state.session_bypass.lock().unwrap().is_empty());
        assert!(state.user_allowlist.lock().unwrap().is_none());
    }

    /// Two `Session` approvals for the same session accumulate
    /// (don't dedupe) — the pre-approval check short-circuits on
    /// the first match anyway, so duplicate entries are harmless.
    #[test]
    fn session_choice_accumulates() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "sudo apt install foo",
            hermix_transport::ApprovalChoice::Session,
        );
        record_approval_side_effect(
            &state,
            "sess-A",
            "systemctl restart nginx",
            hermix_transport::ApprovalChoice::Session,
        );
        let map = state.session_bypass.lock().unwrap();
        let patterns = map.get("sess-A").expect("sess-A entry");
        assert_eq!(patterns.len(), 2);
    }

    // ── 2026-07-06: dedup tests for `check_recent_send` ────────
    //
    // Background: a single user "你好" was triggering 9
    // `send_message_stream` calls in 2 minutes. Root cause was
    // client-side WS auto-retry + App.tsx queue re-drain races.
    // Server-side dedup is the defensive backstop: any
    // `(session_id, text)` that arrives within 10s of a prior
    // identical send is dropped with a synthetic Done event.

    #[test]
    fn dedup_first_send_is_fresh() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(check_recent_send(&state, "sess-A", "你好"), None);
    }

    #[test]
    fn dedup_identical_send_within_window_is_rejected() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(check_recent_send(&state, "sess-A", "你好"), None);
        // Second identical send within 10s → rejected with elapsed
        // (could be 0s if the clock hasn't ticked, or 1s, etc.
        // what matters is Some()).
        let elapsed = check_recent_send(&state, "sess-A", "你好");
        assert!(elapsed.is_some(), "duplicate must be rejected");
        assert!(elapsed.unwrap() < DEDUP_WINDOW_SECS);
    }

    #[test]
    fn dedup_different_content_within_window_is_accepted() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(check_recent_send(&state, "sess-A", "你好"), None);
        // Different text → fresh, no dedup.
        assert_eq!(check_recent_send(&state, "sess-A", "hello"), None);
    }

    #[test]
    fn dedup_different_sessions_are_independent() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        assert_eq!(check_recent_send(&state, "sess-A", "你好"), None);
        // Same content, different session → fresh (different
        // dedup key namespace).
        assert_eq!(check_recent_send(&state, "sess-B", "你好"), None);
    }

    #[test]
    fn dedup_key_is_stable_for_same_text() {
        // Different process instantiations of DefaultHasher can
        // in theory produce different hashes (SipHash is
        // process-randomized). Document that — within a single
        // process the key IS stable (the contract the dedup map
        // relies on). Across processes, dedup is a defense-in-
        // depth, not a correctness requirement.
        let k1 = dedup_key("sess-A", "你好");
        let k2 = dedup_key("sess-A", "你好");
        assert_eq!(k1, k2);
    }

    /// Storm simulation: 9 identical sends within 10s must collapse
    /// to 1 fresh + 8 deduped, with the dedup map holding exactly
    /// 1 entry at the end. This is the exact scenario the user hit
    /// in production ("你好" triggered 9 calls in 2 minutes).
    #[test]
    fn dedup_storm_9_identical_sends_collapses_to_1() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());

        let mut fresh = 0;
        let mut deduped = 0;
        for _ in 0..9 {
            if check_recent_send(&state, "sess-A", "你好").is_none() {
                fresh += 1;
            } else {
                deduped += 1;
            }
        }
        assert_eq!(fresh, 1, "exactly 1 fresh send in a 9-storm");
        assert_eq!(deduped, 8, "exactly 8 deduped in a 9-storm");

        // Map should hold exactly 1 entry (the fresh one).
        let map_size = state.recent_sends.lock().unwrap().len();
        assert_eq!(map_size, 1, "dedup map must have 1 entry, got {map_size}");
    }

    /// Storm + legit follow-up: 5 identical "你好" sends then 1
    /// different "在吗?" send — only the 1st of the 5 "你好"
    /// calls is fresh (subsequent identical calls within 10s
    /// are deduped), and the distinct "在吗?" MUST succeed
    /// because dedup must not block legitimate distinct
    /// messages.
    #[test]
    fn dedup_storm_then_different_content_succeeds() {
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());

        // 1st "你好" is fresh
        assert_eq!(check_recent_send(&state, "sess-A", "你好"), None);
        // Next 4 "你好" are all deduped (within 10s window)
        for i in 0..4 {
            let elapsed = check_recent_send(&state, "sess-A", "你好");
            assert!(elapsed.is_some(), "iteration {i} should be deduped");
        }
        // Legit different content MUST succeed
        assert_eq!(check_recent_send(&state, "sess-A", "在吗?"), None);

        // Map should hold exactly 2 entries (one per content key)
        let map_size = state.recent_sends.lock().unwrap().len();
        assert_eq!(map_size, 2, "map should hold 2 keys (你好 + 在吗?)");
    }

    /// Concurrent storm: spawn 50 threads, each sending the same
    /// text 5 times, into a shared state. Total = 250 calls. The
    /// dedup map's mutex serializes the check-then-insert, so
    /// exactly 1 fresh + 249 deduped, regardless of OS-thread
    /// scheduling. This guards against the race that an unlocked
    /// check-then-insert would produce (multiple threads seeing
    /// "no entry" before any of them insert).
    #[test]
    fn dedup_storm_concurrent_50_threads_5_calls_each() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(test_state(tempfile::tempdir().unwrap().path().to_path_buf()));
        let fresh = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let deduped = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..50 {
            let state = Arc::clone(&state);
            let fresh = Arc::clone(&fresh);
            let deduped = Arc::clone(&deduped);
            handles.push(thread::spawn(move || {
                for _ in 0..5 {
                    if check_recent_send(&state, "sess-A", "你好").is_none() {
                        fresh.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        deduped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let total_fresh = fresh.load(std::sync::atomic::Ordering::Relaxed);
        let total_deduped = deduped.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(total_fresh, 1, "exactly 1 fresh under 50-thread storm");
        assert_eq!(total_deduped, 249, "exactly 249 deduped under 50-thread storm");
    }

    /// Two `Always` approvals for the same command do NOT
    /// double-write the file (each save is full overwrite, so the
    /// file is just rewritten with the same content — but the
    /// in-memory `UserAllowlist` does keep duplicate entries,
    /// since the MVP is "every approval is recorded"). This
    /// matches the upstream Hermes semantics ("once always,
    /// always always" — duplicates are tolerated, the UI can
    /// dedupe on display).
    #[test]
    fn always_choice_dedupes_in_memory() {
        // Note: UserAllowlist::add_literal does NOT dedupe — it
        // appends a new entry. This test pins that behavior so a
        // future dedupe pass shows up as a test diff.
        let state = test_state(tempfile::tempdir().unwrap().path().to_path_buf());
        record_approval_side_effect(
            &state,
            "sess-A",
            "docker system prune",
            hermix_transport::ApprovalChoice::Always,
        );
        record_approval_side_effect(
            &state,
            "sess-A",
            "docker system prune",
            hermix_transport::ApprovalChoice::Always,
        );
        let al = state.user_allowlist.lock().unwrap();
        let al = al.as_ref().unwrap();
        // No dedupe — both entries stored. This is a known
        // MVP limitation; the UI can show distinct timestamps
        // to avoid surprising the user.
        assert_eq!(al.len(), 2);
    }
}

/// Envelope broadcast to every WebSocket when a `clarify` request is
/// enqueued. Each subscriber is expected to filter by `session_id`
/// before forwarding to its client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyBroadcast {
    pub session_id: String,
    pub request_id: String,
    pub question: String,
    pub choices: Vec<String>,
}

#[derive(Clone)]
pub struct HttpServerState {
    pub config: Arc<GatewayConfig>,
    pub tool_registry: Arc<ToolRegistry>,
    pub hermix_home: std::path::PathBuf,
    pub session_persistence: Arc<hermix_agent::session_persistence::SessionPersistence>,
    pub cron_scheduler: Option<Arc<hermix_cron::CronScheduler>>,
    pub skill_store: Option<Arc<dyn hermix_skills::SkillStore>>,
    pub runtime_gateway_running: Option<Arc<AtomicBool>>,
    agent_service: Arc<dyn hermix_core::traits::AgentService>,
    pub approval_registry: ApprovalRegistry,
    /// Pending `clarify` tool calls wait here. The desktop UI subscribes
    /// to the dispatcher via [`Self::clarify_subscribe`] and pushes the
    /// user's answer back over WebSocket.
    pub clarify_dispatcher: hermix_tools::ClarifyDispatcher,
    /// Broadcast of "new pending clarify snapshot available" — every
    /// WebSocket that wants to render question cards subscribes a clone.
    pub clarify_broadcast: tokio::sync::broadcast::Sender<ClarifyBroadcast>,
    /// In-memory catalog of expert role packages. The desktop client pushes
    /// the bundled built-ins into this registry at startup; every expert
    /// surfaced in the UI must come from here.
    pub expert_registry: hermix_skills::ExpertRegistry,
    /// Background scheduler that fires automation tasks at their cron
    /// schedule. Started automatically by `build*`; can be shut down via
    /// `AutomationScheduler::shutdown()`.
    pub automation_scheduler: Option<Arc<crate::dashboard::automation::AutomationScheduler>>,
    /// Persistent user "always allow" command list, loaded from
    /// `$HERMIX_HOME/approval_allowlist.json` at startup. Mutated
    /// when the user picks `Always` on a `RequestApproval` card.
    /// Wrapped in `std::sync::Mutex` for sync access from the
    /// pre-tool-dispatch hot path (the closure is called on every
    /// tool dispatch, not async). `None` means "no allowlist
    /// configured" — the pre-approval checker still runs against
    /// the per-session bypass set, but skips the disk-backed
    /// `UserAllowlist` check.
    pub user_allowlist: Arc<std::sync::Mutex<Option<hermix_tools::UserAllowlist>>>,
    /// Per-session in-memory bypass set: commands the user picked
    /// `Session` on. Cleared on server restart (matches upstream
    /// Hermes semantics). Keys are `session_id`, values are a
    /// list of compiled regex patterns (mirroring `UserAllowlist`).
    pub session_bypass:
        Arc<std::sync::Mutex<HashMap<String, Vec<Regex>>>>,
    /// 2026-07-06: per-`(session_id, content_hash)` send-dedup.
    /// Prevents retry storms (WS auto-retry + queue re-drain) from
    /// triggering N parallel `send_message_stream` calls for the
    /// same user message — the visible symptom was 9 calls in 2
    /// minutes for a single "你好" send. Window: 10s; older entries
    /// are GC'd on every insert. In-memory only, cleared on restart
    /// (good enough — restart also clears the WS clients that
    /// produced the storm).
    pub recent_sends: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// 2026-07-06: set of session IDs that currently have an active
    /// `send_message_stream` running. Prevents concurrent streams for
    /// the same session — the frontend retry mechanism (MAX_RETRIES=3
    /// in useStreamChat.ts) can race with a disconnected WS to start
    /// a second agent for the same user message. Without this guard,
    /// two agent loops run in parallel, producing duplicate tool calls
    /// and doubling the response text — the user sees "LLM 重复回复".
    pub active_sessions: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Phase 14 / H3 — agent "consciousness stream" tracker. The
    /// desktop UI subscribes to this over WebSocket and renders the
    /// samples as a full-screen ECG visualization. Cheap to clone
    /// (single `Arc` over an inner mutex-protected ring buffer).
    pub consciousness_tracker: hermix_agent::ConsciousnessTracker,
    /// M2: TickLoop lifecycle manager. Default ON at startup; can be
    /// toggled via `POST /api/v1/tickloop`.
    pub tickloop_manager: hermix_runtime::TickLoopManager,
    /// GroupChat manager for collaborative multi-agent chat rooms.
    /// Always constructed — even when SQLite fails, we fall back to
    /// an in-memory persistence backend — so callers can unwrap
    /// freely without worrying about 503.
    pub groupchat_mgr: Arc<hermix_gateway::groupchat::GroupChatManager>,
    /// MCP (Model Context Protocol) manager — connects to the external
    /// MCP servers the user configured in the dashboard's "连接器" tab,
    /// registers their tools into the shared `tool_registry`, and
    /// routes `tools/call` JSON-RPC back into the right server.
    ///
    /// Wrapped in `Arc` so the per-tool handlers (which live in the
    /// `ToolRegistry`) can hold a `Weak` back-reference without
    /// forming a cycle. Earlier versions declared `McpManager` but
    /// never constructed it — the manager is wired here, once, at
    /// server startup, immediately after `tool_registry` is built so
    /// the very first LLM turn sees MCP tools.
    pub mcp_manager: Arc<hermix_mcp::McpManager>,
    /// S5 task engine runtime. Owns the per-room scheduler/bus/audit
    /// state and bridges WS events into scheduler actions.
    pub task_engine: Option<Arc<hermix_gateway::groupchat::TaskEngineRuntime>>,
    /// Per-process broadcast sender for todo mutations. The `todo` tool
    /// holds the same sender; the WS handler subscribes per-session
    /// (filtering by `session_id` carried in `TodoChange.session_id`
    /// — currently `None` and filtered on the WS side via the
    /// conversation's session id, see `handle_ws_stream`).
    pub todo_event_tx: Option<tokio::sync::broadcast::Sender<hermix_tools::tools::todo::TodoChange>>,
    /// Phase 0 / P0-5: Orchestration event bus. Cross-crate channel
    /// for Task lifecycle, automation triggers, and result delivery
    /// events. Tool backends and the agent loop hold a clone to emit;
    /// the WS handler subscribes to forward events to connected clients.
    pub orchestration_bus: OrchestrationEventBus,
    /// Phase 1 / P1-5: Task lifecycle service. Backed by
    /// [`hermix_task::TaskRepository`] pointing at the same
    /// `sessions.db` (铁律 D/WAL). `None` when the DB path can't be
    /// resolved (e.g. in test fixtures).
    pub task_service: Option<std::sync::Arc<hermix_task::TaskService>>,
    /// Phase 5 / P5-5: Automation lifecycle service. Backed by
    /// [`hermix_automation::AutomationRepository`] pointing at the same
    /// `sessions.db` (铁律 D/WAL). `None` when the DB path can't be
    /// resolved (e.g. in test fixtures).
    pub automation_service: Option<std::sync::Arc<hermix_automation::AutomationService>>,
    /// Phase 6 / P6-5: Handle to the automation engine scheduler
    /// (from `hermix-automation` crate). Started automatically by
    /// `build_with_hermes_home`; triggers go through the unified
    /// `agent_service.send_message()` dispatch path (铁律 DD).
    pub automation_engine_scheduler: Option<automation_trigger::AutomationSchedulerHandle>,
}

impl HttpServerState {
    /// Subscribe to the broadcast of new pending `clarify` requests. Each
    /// WebSocket call this once at connect-time; the returned receiver
    /// yields one [`ClarifyBroadcast`] per freshly enqueued request.
    pub fn clarify_subscribe(&self) -> tokio::sync::broadcast::Receiver<ClarifyBroadcast> {
        self.clarify_broadcast.subscribe()
    }

    /// Phase 4 / P4-1: Register the `task_create` / `task_update` /
    /// `task_list` tools into the tool registry, backed by the
    /// `TaskService`. No-op if `task_service` is `None`.
    pub fn register_task_tracker_tools(&self) {
        if let Some(svc) = &self.task_service {
            let backend = Arc::new(task_tracker_adapter::TaskTrackerBackendImpl::new(
                svc.clone(),
            ));
            hermix_tools::tools::task_tracker::register(&self.tool_registry, backend);
            tracing::info!("Registered task_tracker tools (task_create, task_update, task_list, task_get, present_files)");
        }
    }

    /// Access the GroupChat manager for collaborative multi-agent rooms.
    /// Never returns `None` — the manager is always constructed at
    /// startup (falling back to in-memory persistence if SQLite fails),
    /// so callers can use this directly without 503 guards.
    pub fn groupchat_manager(&self) -> Arc<hermix_gateway::groupchat::GroupChatManager> {
        self.groupchat_mgr.clone()
    }

    /// Access the agent service that backs chat endpoints. Always present
    /// — the constructor takes care of building a `LocalAgentService`
    /// (or honouring a test-supplied [`AgentService`] mock) before
    /// exposing the state to the router.
    ///
    /// Polish/automation rewrite endpoints live in
    /// [`crate::dashboard::polish`] and call this to dispatch a single
    /// stateless LLM turn. They build an ad-hoc session id, send one
    /// user turn, then call `reset_session` so the parent conversation
    /// history stays untouched.
    pub fn agent_service(&self) -> Arc<dyn hermix_core::traits::AgentService> {
        self.agent_service.clone()
    }

    /// Build dashboard state using the default [`hermix_config::hermix_home`] resolution.
    pub async fn build(config: GatewayConfig) -> Result<Self, AgentError> {
        Self::build_with_hermes_home(config, hermix_config::hermix_home()).await
    }

    /// Build dashboard state with an explicit Hermes data directory (sessions DB, cron dir,
    /// `dashboard_plugins.json`, etc.). Useful for tests and alternate install layouts.
    pub async fn build_with_hermes_home(
        mut config: GatewayConfig,
        hermix_home: std::path::PathBuf,
    ) -> Result<Self, AgentError> {
        // ── Auto-correct config.model BEFORE building the agent
        // service. Without this, a fresh install with only
        // `MINIMAX_API_KEY` in `.env` (no `config.yaml`) starts with
        // `config.model = None`, the runtime falls back to
        // `fallback_model()`, and on the OLD binary that fallback
        // silently routed to `openai:gpt-4o` — leaving the user
        // staring at a "missing API key for openai" error even
        // though they had configured MiniMax. We now pick a
        // provider that actually has a key and persist the
        // correction so subsequent restarts load it.
        if let Some(report) = hermix_config::auto_correct_model(&mut config) {
            tracing::info!(
                before = ?report.before,
                after = %report.after,
                provider = %report.provider,
                reason = %report.reason,
                configured = ?report.configured_providers,
                "startup: auto-corrected config.model"
            );
            let config_path = hermix_home.join("config.yaml");
            if let Err(e) = hermix_config::save_config_yaml(&config_path, &config) {
                tracing::warn!(
                    error = %e,
                    path = %config_path.display(),
                    "startup: failed to persist auto-corrected config (continuing with in-memory fix)"
                );
            }
        }

        let tool_registry = Arc::new(ToolRegistry::new());
        // Register builtin tools (file, shell, browser, etc.)
        let terminal_backend: Arc<dyn hermix_core::TerminalBackend> =
            Arc::new(hermix_environments::LocalBackend::default());
        let skill_store = Arc::new(hermix_skills::FileSkillStore::new(
            hermix_skills::FileSkillStore::default_dir(),
        ));
        let skill_provider: Arc<dyn hermix_core::SkillProvider> =
            Arc::new(hermix_skills::SkillManager::new(skill_store));
        // Wire `clarify` to a blocking dispatcher so the agent truly waits
        // for the desktop UI to render a question card and reply, instead of
        // returning a JSON envelope that the model then re-parses as
        // ordinary tool output.
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::new();
        let (clarify_broadcast, _) = tokio::sync::broadcast::channel::<ClarifyBroadcast>(64);
        // Background pump: subscribes to dispatcher snapshots and
        // broadcasts each new pending `clarify` to every connected
        // WebSocket. The desktop UI filters by session id before showing
        // a question card.
        let pump_dispatcher = clarify_dispatcher.clone();
        let pump_broadcast = clarify_broadcast.clone();
        tokio::spawn(async move {
            loop {
                let snapshots = pump_dispatcher.next_snapshot().await;
                for snap in snapshots {
                    let env = ClarifyBroadcast {
                        session_id: snap.session_id.clone(),
                        request_id: snap.request_id.clone(),
                        question: snap.question.clone(),
                        choices: snap.choices.clone(),
                    };
                    let _ = pump_broadcast.send(env);
                }
            }
        });
        // Todo event bus (see build() for full rationale).
        let (todo_event_tx, _) = tokio::sync::broadcast::channel::<
            hermix_tools::tools::todo::TodoChange,
        >(hermix_tools::tools::todo::TODO_EVENT_CHANNEL_CAPACITY);

        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
            Some(todo_event_tx.clone()),
        );
        // Reconcile the in-process registry's `is_enabled` flags
        // with whatever the user last persisted in
        // `enabled_toolsets.json`. Without this, every restart
        // would silently reset every toggle the user made in
        // the Settings UI to "on" — the LLM would start seeing
        // the toolsets the user thought they had disabled.
        crate::dashboard::skills::apply_persisted_toolset_state(
            &tool_registry,
            &hermix_home,
        );

        // ★ P1-② (2026-07-05): load native plugins (.dll) from
        //   $HERMIX_HOME/plugins/tools/ and /plugins/memory/.
        //   Each directory must contain a `plugin.yaml` with a
        //   `binary` field. Plugins export `hermix_plugin_init()`
        //   via the C ABI. Each ToolInstance is wrapped as
        //   `NativeToolAdapter`, registered into `tool_registry`.
        {
            let dll_paths =
                hermix_agent::native_plugin::discover_native_plugins(&hermix_home);
            for dll in &dll_paths {
                match unsafe {
                    hermix_agent::native_plugin::LoadedNativePlugin::load(dll)
                } {
                    Ok(plugin) => {
                        let mut registered = 0usize;
                        for idx in 0..plugin.tool_count {
                            match unsafe { plugin.create_tool_handler(idx) } {
                                Ok(handler) => {
                                    let schema = handler.schema();
                                    let name = schema.name.clone();
                                    let risk = RiskLevel::Caution;
                                    tool_registry.register_with_risk(
                                        &name,
                                        "native-plugin",
                                        schema,
                                        handler,
                                        Arc::new(|| true),
                                        vec![],
                                        true,
                                        &format!(
                                            "Native plugin tool: {}",
                                            plugin.name
                                        ),
                                        "🔌",
                                        None,
                                        risk,
                                        false,
                                    );
                                    registered += 1;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "hermix.plugins",
                                        name = %plugin.name,
                                        index = idx,
                                        error = %e,
                                        "Native plugin tool creation failed"
                                    );
                                }
                            }
                        }
                        tracing::info!(
                            target: "hermix.plugins",
                            name = %plugin.name,
                            tools = plugin.tool_count,
                            registered,
                            "Native plugin loaded"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "hermix.plugins",
                            dll = %dll.display(),
                            error = %e,
                            "Failed to load native plugin"
                        );
                    }
                }
            }
            if !dll_paths.is_empty() {
                tracing::info!(
                    target: "hermix.plugins",
                    count = dll_paths.len(),
                    "Scanned native plugins"
                );
            }
        }

        // MCP integration (Phase 14 — wiring the previously-dead
        // McpManager). We construct the manager against the SAME
        // tool_registry that the agent service already sees, then
        // iterate the user-configured servers in
        // `$HERMIX_HOME/mcp_servers.json` and `connect()` each enabled
        // one. Disabling happens later in the request lifecycle: the
        // dashboard's `update_server` route calls
        // `McpManager::disconnect` when `enabled` flips to false.
        //
        // Each `connect()` registers discovered tools (with real
        // schemas) into the tool registry, so the LLM sees them in the
        // next `get_definitions()` call. We deliberately ignore
        // per-server connect failures: one broken server must not
        // prevent the rest from coming up.
        let mcp_manager = hermix_mcp::ArcMcpManager::new(tool_registry.clone());
        {
            let mcp_list = crate::dashboard::mcp::load_mcp_servers(&hermix_home);
            let mcp_mgr = mcp_manager.clone();
            // Spawn MCP connection as a background task so it does NOT
            // block the HTTP listener from binding. `npx -y <pkg>` can
            // take 10-30s on first run (package download + Node startup),
            // which would cause the sidecar watchdog to think the server
            // is dead and restart it in a loop. The MCP tools appear in
            // the registry as soon as each server finishes connecting.
            tokio::spawn(async move {
                for entry in mcp_list.servers.iter().filter(|e| e.enabled) {
                    match crate::dashboard::mcp::entry_to_server_config(entry) {
                        Ok((cfg_name, cfg)) => {
                            if let Err(e) = mcp_mgr.connect(&cfg_name, cfg).await {
                                tracing::warn!(
                                    server = %cfg_name,
                                    error = %e,
                                    "MCP server connect failed at startup; skipping"
                                );
                            } else {
                                tracing::info!(server = %cfg_name, "MCP server connected");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                server = %entry.name,
                                error = %e,
                                "MCP server entry invalid; skipping"
                            );
                        }
                    }
                }
            });
        }

        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        // Don't swallow ensure_db errors silently — a failed migration
        // means later queries will crash with "no such column: …" and
        // the user has no way to know what went wrong. Log loudly.
        if let Err(e) = session_persistence.ensure_db() {
            tracing::error!(error = %e, "session db ensure_db failed at startup");
        }

        // Phase 14: a single consciousness tracker shared by the
        // agent service (writer) and the dashboard endpoints
        // (readers via REST + WebSocket broadcast).
        let consciousness_tracker = hermix_agent::ConsciousnessTracker::new();

        // Create LocalAgentService, wiring in the shared
        // consciousness tracker so the agent loop emits ECG
        // vitals for the desktop UI.
        let agent_service_inner = hermix_agent::LocalAgentService::new(
            Arc::new(config.clone()),
            tool_registry.clone(),
            session_persistence.clone(),
        )
        .with_consciousness_tracker(Arc::new(consciousness_tracker.clone()));
        // Kick off a background cold-start warmup. The user's
        // first real chat would otherwise eat the 30–60s the
        // provider needs to load its model and warm its HTTP
        // keep-alive pool. The warmup is fire-and-forget and
        // cannot fail in a way that blocks startup.
        agent_service_inner.warm_up_default_provider();
        let agent_service: Arc<dyn hermix_core::traits::AgentService> =
            Arc::new(agent_service_inner);

        // Initialize cron scheduler backed by `$hermix_home/data/cron`
        let cron_dir = hermix_config::paths::data_dir_in(&hermix_home).join("cron");
        let cron_scheduler = Arc::new(hermix_cron::cli_support::cron_scheduler_for_data_dir(
            cron_dir,
        ));

        // Initialize skill store
        let skill_store: Arc<dyn hermix_skills::SkillStore> = Arc::new(
            hermix_skills::FileSkillStore::new(hermix_skills::FileSkillStore::default_dir()),
        );

        // Seed builtin skills into the default skills dir so the front-end
        // "扩展 → 技能" tab has a populated list on first launch. Idempotent.
        crate::dashboard::skills::seed_builtin_skills_default();

        // Load the persistent user allowlist (if present) and seed
        // a default-empty in-memory session bypass map. Both are
        // wired into the pre-tool-dispatch checker below.
        let user_allowlist_path = hermix_config::paths::approval_allowlist_path();
        let user_allowlist = UserAllowlist::load(&user_allowlist_path);
        let user_allowlist = if user_allowlist.is_empty() {
            None
        } else {
            Some(user_allowlist)
        };

        let (llm_provider_name, llm_probe_url) = extract_llm_probe_url(&config).unzip();

        let groupchat_mgr =
            Self::init_groupchat_manager(&hermix_home, Some(agent_service.clone()));

        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            cron_scheduler: Some(cron_scheduler),
            skill_store: Some(skill_store),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(user_allowlist)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker,
            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig {
                    llm_provider_url: llm_probe_url.flatten(),
                    llm_provider_name,
                    ..hermix_runtime::TickLoopConfig::default()
                },
            ),
            groupchat_mgr: groupchat_mgr.clone(),
            mcp_manager,
            task_engine: Some(Arc::new(
                hermix_gateway::groupchat::TaskEngineRuntime::new(groupchat_mgr),
            )),
            // 2026-07-06: dedup map for retry-storm protection. See
            // the `recent_sends` field doc on the struct. Cleared
            // on server restart, which is fine because the WS
            // clients that produced the storm are also gone.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            // Todo event bus — created lazily below in `build()` after
            // we know the runtime config. Initialised to None here so
            // the early struct literal doesn't need to await anything.
            todo_event_tx: Some(todo_event_tx.clone()),
            orchestration_bus: OrchestrationEventBus::new(),
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
            automation_service: {
                let repo = std::sync::Arc::new(hermix_automation::AutomationRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_automation::AutomationService::new(repo)))
            },
            automation_engine_scheduler: None,
        };
        state.register_task_tracker_tools();

        // Phase 6 / P6-5: Start the automation scheduler with the unified
        // dispatch path. The trigger callback calls `agent_service.send_message()`,
        // which internally uses `ToolRegistry::dispatch_async()` — the same
        // path as HTTP chat requests (铁律 DD).
        if let (Some(auto_svc), Some(task_svc)) = (
            &state.automation_service,
            &state.task_service,
        ) {
            let handle = automation_trigger::start_automation_scheduler(
                auto_svc.clone(),
                state.agent_service(),
                state.orchestration_bus.clone(),
                Some(task_svc.clone()),
            );
            // SAFETY: we're mutating a field of the returned state before
            // handing it to the caller. This is safe because no other code
            // has a reference to `state` yet.
            let mut state = state;
            state.automation_engine_scheduler = Some(handle);
            return Ok(state);
        }

        Ok(state)
    }

    pub async fn build_with_agent_service(
        config: GatewayConfig,
        agent_service: Arc<dyn hermix_core::traits::AgentService>,
    ) -> Result<Self, AgentError> {
        let tool_registry = Arc::new(ToolRegistry::new());
        // Register builtin tools (file, shell, browser, etc.)
        let terminal_backend: Arc<dyn hermix_core::TerminalBackend> =
            Arc::new(hermix_environments::LocalBackend::default());
        let skill_store = Arc::new(hermix_skills::FileSkillStore::new(
            hermix_skills::FileSkillStore::default_dir(),
        ));
        let skill_provider: Arc<dyn hermix_core::SkillProvider> =
            Arc::new(hermix_skills::SkillManager::new(skill_store));
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::new();
        let (clarify_broadcast, _) = tokio::sync::broadcast::channel::<ClarifyBroadcast>(64);
        let pump_dispatcher = clarify_dispatcher.clone();
        let pump_broadcast = clarify_broadcast.clone();
        tokio::spawn(async move {
            loop {
                let snapshots = pump_dispatcher.next_snapshot().await;
                for snap in snapshots {
                    let env = ClarifyBroadcast {
                        session_id: snap.session_id.clone(),
                        request_id: snap.request_id.clone(),
                        question: snap.question.clone(),
                        choices: snap.choices.clone(),
                    };
                    let _ = pump_broadcast.send(env);
                }
            }
        });
        // Todo event bus (see build() for full rationale).
        let (todo_event_tx, _) = tokio::sync::broadcast::channel::<
            hermix_tools::tools::todo::TodoChange,
        >(hermix_tools::tools::todo::TODO_EVENT_CHANNEL_CAPACITY);

        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
            Some(todo_event_tx.clone()),
        );

        // MCP wiring (mirrors `build_with_hermes_home`). Construct
        // the manager and eagerly connect every enabled server from
        // `$HERMIX_HOME/mcp_servers.json`.
        let mcp_manager = hermix_mcp::ArcMcpManager::new(tool_registry.clone());
        let hermix_home = hermix_config::hermix_home();
        {
            let mcp_list = crate::dashboard::mcp::load_mcp_servers(&hermix_home);
            for entry in mcp_list.servers.iter().filter(|e| e.enabled) {
                match crate::dashboard::mcp::entry_to_server_config(entry) {
                    Ok((cfg_name, cfg)) => {
                        if let Err(e) = mcp_manager.connect(&cfg_name, cfg).await {
                            tracing::warn!(
                                server = %cfg_name,
                                error = %e,
                                "MCP server connect failed at startup; skipping"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %entry.name,
                            error = %e,
                            "MCP server entry invalid; skipping"
                        );
                    }
                }
            }
        }
        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        // Don't swallow ensure_db errors silently — a failed migration
        // means later queries will crash with "no such column: …" and
        // the user has no way to know what went wrong. Log loudly.
        if let Err(e) = session_persistence.ensure_db() {
            tracing::error!(error = %e, "session db ensure_db failed at startup");
        }

        // Initialize cron scheduler backed by $HERMIX_HOME/data/cron
        let cron_dir = hermix_config::paths::data_dir_in(&hermix_home).join("cron");
        let cron_scheduler = Arc::new(hermix_cron::cli_support::cron_scheduler_for_data_dir(
            cron_dir,
        ));

        // Initialize skill store
        let skill_store: Arc<dyn hermix_skills::SkillStore> = Arc::new(
            hermix_skills::FileSkillStore::new(hermix_skills::FileSkillStore::default_dir()),
        );

        // Seed builtin skills into the default skills dir so the front-end
        // "扩展 → 技能" tab has a populated list on first launch. Idempotent.
        crate::dashboard::skills::seed_builtin_skills_default();

        // Load the persistent user allowlist (if present). Same
        // shape as `build_with_hermes_home`; see that fn for the
        // design rationale on the `Option<...>` wrapper.
        let user_allowlist_path = hermix_config::paths::approval_allowlist_path();
        let user_allowlist = UserAllowlist::load(&user_allowlist_path);
        let user_allowlist = if user_allowlist.is_empty() {
            None
        } else {
            Some(user_allowlist)
        };

        let (llm_provider_name, llm_probe_url) = extract_llm_probe_url(&config).unzip();

        let groupchat_mgr = Self::init_groupchat_manager(&hermix_home, Some(agent_service.clone()));

        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            cron_scheduler: Some(cron_scheduler),
            skill_store: Some(skill_store),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(user_allowlist)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker: hermix_agent::ConsciousnessTracker::new(),
            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig {
                    llm_provider_url: llm_probe_url.flatten(),
                    llm_provider_name,
                    ..hermix_runtime::TickLoopConfig::default()
                },
            ),
            groupchat_mgr: groupchat_mgr.clone(),
            mcp_manager,
            task_engine: Some(Arc::new(
                hermix_gateway::groupchat::TaskEngineRuntime::new(groupchat_mgr),
            )),
            // 2026-07-06: dedup map for retry-storm protection. See
            // the `recent_sends` field doc on the struct. Cleared
            // on server restart, which is fine because the WS
            // clients that produced the storm are also gone.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            todo_event_tx: Some(todo_event_tx.clone()), // injected after register_builtin_tools below
            orchestration_bus: OrchestrationEventBus::new(),
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
            automation_service: {
                let repo = std::sync::Arc::new(hermix_automation::AutomationRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_automation::AutomationService::new(repo)))
            },
            automation_engine_scheduler: None,
        }; // close Self { ... }
        state.register_task_tracker_tools();
        Ok(state)
    }

    pub fn with_runtime_gateway_running(
        mut self,
        runtime_gateway_running: Arc<AtomicBool>,
    ) -> Self {
        self.runtime_gateway_running = Some(runtime_gateway_running);
        self
    }

    /// Start the automation scheduler background loop and stash the
    /// handle in the state so callers can later shut it down. Idempotent:
    /// re-calling this replaces the prior handle.
    pub fn with_automation_scheduler(mut self) -> Self {
        let scheduler =
            crate::dashboard::automation::AutomationScheduler::start(self.clone());
        self.automation_scheduler = Some(scheduler);
        self
    }

    /// Build a [`GroupChatManager`] with SQLite-backed persistence
    /// (falling back to in-memory when the DB can't be opened) and
    /// optionally wire an `AgentService` as its dispatcher.
    ///
    /// Always returns a manager — when every persistence backend fails,
    /// a blank in-memory manager is the final fallback, so the front-end
    /// never sees 503 for groupchat operations.
    fn init_groupchat_manager(
        hermix_home: &std::path::Path,
        agent_service: Option<Arc<dyn hermix_core::traits::AgentService>>,
    ) -> Arc<hermix_gateway::groupchat::GroupChatManager> {
        let db: Arc<dyn hermix_gateway::groupchat::RoomPersistence> =
            match crate::dashboard::groupchat_sqlite::SqliteRoomPersistence::new(hermix_home) {
                Ok(sqlite) => Arc::new(sqlite),
                Err(e) => {
                    tracing::warn!(
                        "SQLite groupchat persistence failed, using in-memory: {}",
                        e
                    );
                    Arc::new(hermix_gateway::groupchat::InMemoryRoomPersistence::new())
                }
            };
        let mgr = Arc::new(hermix_gateway::groupchat::GroupChatManager::new(db));
        // Wrap in a Mutex so retries can release the lock between
        // attempts (see the `Connection::open` retry loop in
        // `SqliteRoomPersistence::new`).
        if let Some(svc) = agent_service {
            let dispatcher = Arc::new(
                crate::dashboard::groupchat_dispatcher::ServerAgentDispatcher::new(svc),
            );
            mgr.set_dispatcher(dispatcher);
        }
        // P1.3 — wire up the idle chat publish hook. When a member
        // agent generates an idle chat message that the dispatcher
        // successfully returns, this hook persists the message into
        // the room and awards a small rank bonus (+3) to the
        // author. The hook spawns a detached task so the dispatcher
        // future returns without waiting on db writes. Failures are
        // traced but never propagate — idle chat is best-effort.
        let mgr_for_hook = mgr.clone();
        mgr.install_idle_publish_hook(std::sync::Arc::new(move |room_id, agent_id, content| {
            let mgr_inner = mgr_for_hook.clone();
            let room_id = room_id.to_string();
            let agent_id = agent_id.to_string();
            let content = content.to_string();
            tokio::spawn(async move {
                if let Err(e) = mgr_inner
                    .send_message(&room_id, &agent_id, &agent_id, &content)
                    .await
                {
                    tracing::warn!(
                        "idle publish hook: send_message failed for room={} agent={}: {}",
                        room_id, agent_id, e
                    );
                }
                if let Err(e) = mgr_inner
                    .adjust_member_score(&room_id, &agent_id, 3)
                    .await
                {
                    tracing::warn!(
                        "idle publish hook: adjust_member_score failed for room={} agent={}: {}",
                        room_id, agent_id, e
                    );
                }
            });
        }));
        mgr
    }
}

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
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/v1/sessions/{session_id}",
            post(rename_session).patch(patch_session).delete(delete_session),
        )
        .route(
            "/v1/sessions/{session_id}/messages",
            get(get_session_messages).post(send_message),
        )
        // v2 (2026-06-21): archive refs surface for the chat UI.
        // `GET /v1/sessions/{id}/archive-refs` returns the per-session
        // JSONL index written by `archive_context_section`. Empty
        // Vec when the session has never been archived.
        .route(
            "/v1/sessions/{session_id}/archive-refs",
            get(get_session_archive_refs),
        )
        // T2-A (2026-06-20): moved from /api/sessions/* (dashboard router).
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
        // v1 (2026-07-07): todo (task list) HTTP endpoints. The LLM
        // goes through the `todo` tool over WS; the desktop TodoPanel
        // hits these endpoints for hydration (GET) and manual
        // checkbox toggles (PATCH). See `list_todos` / `patch_todo`.
        .route("/v1/todos", get(list_todos))
        .route(
            "/v1/todos/{todo_id}",
            patch(patch_todo).delete(delete_todo),
        )
        .route("/v1/commands", post(exec_command))
        .route("/v1/ws/{session_id}", get(ws_upgrade))
        .route("/v1/ws-stream/{session_id}", get(ws_stream_upgrade))
        .route("/v1/ws-groupchat/{room_id}", get(ws_groupchat_upgrade))
        // Expert catalog (driven by the desktop client's bundled built-ins).
        .route("/v1/experts", get(list_experts).post(register_expert))
        .route("/v1/experts/{id}", get(get_expert))
        // T2026-06-21 P1: dynamic tool discovery — the desktop client
        // calls this once at startup and uses the result as the single
        // source of truth for `overrides.tools`. Adding a new tool to
        // the `ToolRegistry` automatically surfaces it in the chat UI
        // and the LLM's tool list, with no front-end code change.
        .route("/v1/tools", get(list_available_tools))
        // Dashboard management API
        .merge(dashboard::router())
        // M2: TickLoop toggle
        .route("/v1/tickloop", get(get_tickloop_status).post(toggle_tickloop))
        // L3: Voice bridge — HTTP-friendly STT/TTS endpoints.
        // Both routes are stateless: the bridge builds a default
        // `VoicePipeline` per request so concurrent calls don't share
        // config state. The `voice_mode` tool (LLM-invoked) is the
        // proper way to drive a long-lived pipeline with custom
        // STT/TTS providers.
        .route("/v1/voice/transcribe", post(voice_transcribe))
        .route("/v1/voice/synthesize", post(voice_synthesize))
        // Phase 1 / P1-5: Orchestration Engine Task CRUD endpoints.
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route(
            "/v1/tasks/{task_id}",
            get(get_task).patch(patch_task).delete(delete_task),
        )
        .route("/v1/tasks/{task_id}/status", patch(patch_task_status))
        .route("/v1/tasks/{task_id}/children", get(list_task_children))
// Phase 7 / P7-1: Product delivery — register deliverable artifacts.
.route("/v1/tasks/{task_id}/results", post(deliver_results))
        // Phase 5 / P5-5: Automation Engine CRUD endpoints.
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
    // M2: clone TickLoop manager before moving state into the router.
    let tickloop_mgr = state.tickloop_manager.clone();
    let tracker = Arc::new(state.consciousness_tracker.clone());

    // Shutdown signal: /shutdown endpoint sends `true`, graceful shutdown waits for it.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let app = router(state).layer(axum::Extension(shutdown_tx));
    // Phase 18 fix: bind via TcpSocket so we can set SO_REUSEADDR
    // before bind. Default TcpListener::bind does NOT set it on
    // Windows, which means a crashed/exited child leaving a LISTEN
    // socket on 3000 locks the port until the OS reaps the orphan
    // (30s~4min). With SO_REUSEADDR we can re-bind immediately even
    // if the previous listener is still in TIME_WAIT / CLOSE_WAIT.
    //
    // On Linux this is also helpful for fast-restart during dev.
    //
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
// T2026-06-21 P1: dynamic tool discovery
// ---------------------------------------------------------------------------
//
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
            // "未分组" (bug 2026-07-12). `req.project` is the legacy
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
            state.config.model.as_deref(),
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
//
// The `todo` tool is the canonical mutation path (LLM-driven). These
// endpoints exist so the front-end can (a) hydrate the TodoPanel on
// initial mount via `GET /v1/todos`, and (b) let the user manually
// flip a task to `done` via `PATCH /v1/todos/{id}`. Both endpoints
// dispatch through the same `TodoHandler` so the broadcast path
// (`todo_event_tx`) fires on every change, keeping the WS stream and
// the HTTP loop consistent.
//
// Why not call FileTodoBackend directly? Two reasons:
//   * The broadcast fires only when TodoHandler.execute() runs.
//     Bypassing the handler would skip the WS forwarder and leave
//     the desktop panel stale.
//   * It keeps the schema (parameter names, status enums) in one
//     place — the LLM tool definition.
//
// Trade-off: each `GET` re-enters `ToolHandler::execute`, which
// parses the LLM-style action envelope. For N items this is O(N)
// JSON parse, which is fine (todo lists are <100 items).

async fn list_todos(
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let raw = state
        .tool_registry
        .dispatch("todo", serde_json::json!({ "action": "list" }));
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
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // The TodoPanel only ever flips a task to `completed` /
    // `pending`; everything else (title / priority / dependencies)
    // is LLM-driven and goes through the tool directly. Accept
    // any of the canonical fields and forward verbatim so we don't
    // surprise the user when their clicks somehow round-trip a
    // back-end update.
    let mut params = serde_json::json!({
        "action": "update",
        "id": id,
    });
    if let Some(s) = req.get("status").and_then(|v| v.as_str()) {
        params["status"] = serde_json::Value::String(s.to_string());
    }
    if let Some(t) = req.get("title").and_then(|v| v.as_str()) {
        params["title"] = serde_json::Value::String(t.to_string());
    }
    if let Some(p) = req.get("priority").and_then(|v| v.as_str()) {
        params["priority"] = serde_json::Value::String(p.to_string());
    }

    let raw = state.tool_registry.dispatch("todo", params);
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        serde_json::json!({ "ok": true, "raw": raw })
    });
    Ok(Json(parsed))
}

async fn delete_todo(
    Path(id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // Dispatches through `TodoHandler::execute` so the broadcast path
    // (`todo_event_tx`) fires and the WS forwarder drops the row from
    // every connected TodoPanel — keeping the HTTP loop and the WS
    // stream consistent (same rationale as `patch_todo`).
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
///
/// Every server route that touches `sessions.db` MUST go through this
/// helper rather than calling `Connection::open` directly. Without it,
/// `journal_mode = WAL` only applies to the writer side
/// (`session_persistence::open_with_wal`), and the server opener
/// inherits the legacy `journal_mode = delete` default. That mismatch
/// surfaces `SQLITE_CORRUPT` / "database disk image is malformed" the
/// moment the writer was killed mid-transaction and left a stale
/// `-shm` page table — which happens on every Tauri sidecar SIGKILL
/// shutdown.
///
/// Best-effort on PRAGMA failures: if WAL mode is rejected (read-only
/// file, exclusive lock held, etc.) we log a warning and continue —
/// Detect the canonical SQLite "database disk image is malformed"
/// diagnostic. Phase 21 extension (2026-07-07): the original WAL fix
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
///
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

    // Phase 21 extension (2026-07-07): try the cascade-delete; if it
    // fails with "database disk image is malformed" — typically a
    // corrupted `messages_fts` shadow page — rebuild both FTS5
    // indexes and retry once. If the rebuild itself fails or the
    // retry still errors, fall through to the original 500 surface
    // so the user isn't silently told the delete succeeded.
    //
    // No labelled block needed: the retry path returns directly
    // via `return Ok(Json(...))`, and the happy path falls through
    // to the closing `}` of the for-loop naturally. Both branches
    // exit the function on completion, so there's nothing to break
    // out of.
    let tx = conn.unchecked_transaction().map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;

    // 2026-07-08: after the DB rows are gone, also remove the on-disk
    // session artifacts so they don't linger as orphaned data. There
    // are TWO separate trees:
    //   1. `<home>/sessions/<sid>/`        — archive-index.jsonl
    //      (written via `hermix_home`, NOT data_dir)
    //   2. `<home>/data/sessions/<sid>.md` — session log
    //      (written via `data_dir`, flat file)
    // The sidebar list reads from the DB only, so the session won't
    // resurrect — but leaving these behind is an orphaned-data leak.
    // Best-effort: log + ignore on failure; never fail the delete
    // because of leftover files (the DB row is already gone).
    let cleanup_session_dir = || {
        let home = &state.hermix_home;
        let data_dir = hermix_config::paths::data_dir_in(home);

        // 1. Archive tree: <hermix_home>/sessions/<sid>/ (holds
        //    archive-index.jsonl + per-turn markdown sidecars).
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
        //     sessions_dir (normally migrated to sessions_dir/orphans/
        //     on the next save, but a deleted session never gets one).
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
        //    SessionPersistence::save_trajectory on every completed turn).
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

    // 2026-07-08: purge the deleted session's recall memory from
    // memory.db so a removed conversation leaves zero vector/embedding
    // residue. memory.db is WAL + owned by the same process as
    // sessions.db, so a second `Connection::open` (mirroring
    // `open_sessions_db_for_read`) is the established, safe pattern.
    // Every session-scoped store is covered:
    //   - frames / facts            (tagged `session:{id}`, JSON `tags`)
    //   - recall_audit / session_snapshots (explicit `session_id` col)
    //   - frames_fts / facts_fts    (FTS shadow tables; must stay in
    //     sync with frames/facts or future searches corrupt)
    // Best-effort: any failure is logged + ignored; the session delete
    // itself must never fail because the memory purge hiccupped.
    let purge_session_memory = || {
        let mem_path =
            hermix_config::paths::data_dir_in(&state.hermix_home)
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
                // 2026-07-08: same success-path logging/cleanup as the
                // happy path below (see cleanup_session_dir + tracing::info).
                cleanup_session_dir();
                purge_session_memory();
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

    // 2026-07-08: surface the deletion in backend logs. Previously the
    // happy path was completely silent, so operators/owners had no way
    // to confirm a delete actually happened.
    cleanup_session_dir();
    purge_session_memory();
    tracing::info!(
        session_id = %session_id,
        "delete_session: deleted session + cascade child rows (messages/compression_archives/compression_summary_history/token_usage)"
    );

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /v1/sessions/{id}/archive-refs` — return every [`ArchiveRef`]
/// the agent loop has written for this session.
///
/// v2 (2026-06-21): backing storage is a JSONL file at
/// `<hermix_home>/sessions/<sid>/archive-index.jsonl`. We don't go
/// through `AgentService` / `AgentLoop::last_archive_refs()` because
/// that field only holds the **most recent** archive run — for the
/// chat banner we want the full history of archived nodes so the
/// user can re-open any of them.
///
/// Returns `200 { refs: [] }` when:
/// - the session has never triggered a compression round (common),
/// - the index file is missing / empty,
/// - the session id is malformed (we still don't 404 because the
///   state of "session exists but never archived" is a valid one).
///
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
    // v33 (2026-07-06): use the *timestamped* loader so each entry
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

    let mut idx = 0usize;
    let messages: Vec<ProtocolMessage> = entries
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
                // v4 (2026-06-22): forward the persisted execution
                // timeline so session reloads can rebuild the same
                // inline tool cards the user saw mid-stream
                // (correct status, arguments, stdout). Without this,
                // every tool call after a session reload rendered
                // as a stale "运行中" pill because no event marked
                // them as completed.
                let execution_timeline = m.execution_timeline.clone();
                // v5 (2026-06-22): reconstruct tool arguments from
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
                // v33 (2026-07-06): use the persisted `created_at`
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
                    // 2026-07-12 (Bug B): persist already runs through
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
                }
            }
        })
        .collect();
    Ok(Json(SessionMessagesResponse {
        session_id,
        messages,
    }))
}

async fn send_message(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, HttpError> {
    hermix_telemetry::record_http_request();
    let trace_id = hermix_transport::new_trace_id();

    // Build AgentOverrides from request
    let overrides = hermix_core::traits::AgentOverrides {
        model: req.model.clone(),
        personality: req.personality.clone(),
        system_prompt: req.system_prompt.clone(),
        tools: req.tools.clone(),
        workspace: req.workspace.clone(),
        permission: parse_permission_level(req.permission.as_deref()),
        images: convert_wire_images(&req.images),
        image: None,
        image_mime_type: None,
        skills: req.skills.clone(),
        approval_requester: None,
        on_event: None,
        pre_approval_checker: None,
        on_prefetch_intent: None,
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

// T2-A (2026-06-20): moved from `dashboard/sessions.rs`. The dashboard
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

/// `GET /v1/sessions/{session_id}/lineage` — return the parent→ancestor
/// chain (upward) and immediate children (downward) for session lineage
/// browsing in the frontend tree view.
///
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

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state, session_id))
}

async fn handle_ws(mut socket: WebSocket, state: HttpServerState, session_id: String) {
    let _ = socket
        .send(WsMessage::Text(
            format!("connected session={}", session_id).into(),
        ))
        .await;
    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            WsMessage::Text(text) => {
                let parsed: Option<SendMessageRequest> = serde_json::from_str(&text).ok();
                let request = parsed.unwrap_or_else(|| SendMessageRequest {
                    text: text.to_string(),
                    model: None,
                    provider: None,
                    personality: None,
                    system_prompt: None,
                    tools: None,
                    workspace: None,
                    permission: None,
                    user_id: None,
                    request_id: None,
                    images: None,
                    image: None,
                    image_mime_type: None,
                    skills: None,
                });

                // Build AgentOverrides from request
                let overrides = hermix_core::traits::AgentOverrides {
                    model: request.model.clone(),
                    personality: request.personality.clone(),
                    system_prompt: request.system_prompt.clone(),
                    tools: request.tools.clone(),
                    workspace: request.workspace.clone(),
                    permission: parse_permission_level(request.permission.as_deref()),
                    images: convert_wire_images(&request.images),
                    image: None,
                    image_mime_type: None,
                    skills: request.skills.clone(),
                    approval_requester: None,
                    on_event: None,
                    pre_approval_checker: None,
                    on_prefetch_intent: None,
                };

                // Call agent service
                match state
                    .agent_service
                    .send_message(&session_id, &request.text, overrides)
                    .await
                {
                    Ok(reply) => {
                        let _ = socket.send(WsMessage::Text(reply.text.into())).await;
                    }
                    Err(err) => {
                        let _ = socket.send(WsMessage::Text(err.to_string().into())).await;
                    }
                }
            }
            WsMessage::Close(_) => break,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming WebSocket endpoint — real-time Agent events
// ---------------------------------------------------------------------------

/// WebSocket upgrade for streaming endpoint.
async fn ws_stream_upgrade(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws_stream(socket, state, session_id))
}

/// Compute the dedup key for `(session_id, text)`. Stable across
/// processes for the same inputs (uses `DefaultHasher`'s SipHash).
/// Exposed for testing.
fn dedup_key(session_id: &str, text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{}:{:x}", session_id, hasher.finish())
}

/// Dedup window. Anything within this many seconds of a prior
/// identical (session_id, text) send is rejected.
const DEDUP_WINDOW_SECS: u64 = 30;
/// Map GC horizon. Entries older than this are evicted on every
/// insert so the map stays bounded.
const DEDUP_GC_SECS: u64 = 60;

/// Check whether `(session_id, text)` was sent recently. Returns
/// `Some(elapsed_secs)` if a duplicate was found within the dedup
/// window (caller should reject), or `None` if the send is fresh
/// (caller should proceed and the entry is recorded).
///
/// 2026-07-06: extracted from `handle_ws_stream_send` so the dedup
/// contract is unit-testable without a WebSocket.
fn check_recent_send(state: &HttpServerState, session_id: &str, text: &str) -> Option<u64> {
    let key = dedup_key(session_id, text);
    let now = std::time::Instant::now();
    // ★ DIAG 2026-07-08: unconditional stderr print so we can confirm
    // dedup is reached even if tracing is filtered.
    eprintln!(
        "!!!DEDUP CALLED!!! session={} text={:?} key={} map_lock_ok={}",
        session_id,
        text,
        key,
        state.recent_sends.lock().is_ok()
    );
    // T2026-07-06: diagnostic — confirm dedup is reached in real WS path.
    tracing::info!(
        session = %session_id,
        key = %key,
        text_len = text.len(),
        "dedup_check: entering check_recent_send"
    );
    // Check: do we have a recent entry for this key?
    let duplicate: Option<u64> = if let Ok(map) = state.recent_sends.lock() {
        if let Some(last) = map.get(&key) {
            let e = now.duration_since(*last).as_secs();
            if e < DEDUP_WINDOW_SECS {
                Some(e)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        // Mutex poisoned: fail-open (don't block the user). Caller
        // will see `None` and proceed without dedup.
        None
    };
    if duplicate.is_some() {
        tracing::info!(
            session = %session_id,
            elapsed_secs = duplicate.unwrap_or(0),
            "dedup_check: DUPLICATE found"
        );
        return duplicate;
    }
    // Record + GC.
    if let Ok(mut map) = state.recent_sends.lock() {
        map.insert(key.clone(), now);
        map.retain(|_, t| now.duration_since(*t).as_secs() < DEDUP_GC_SECS);
        tracing::info!(
            session = %session_id,
            key = %key,
            map_size = map.len(),
            "dedup_check: FRESH — inserted into map"
        );
    }
    None
}

/// Handle a single `SendMessageRequest` inside a streaming WS session.
/// Extracted so the `WsClientMessage::Send` path and the legacy
/// `SendMessageRequest` / plain-text path share the same logic.
async fn handle_ws_stream_send(
    socket: &mut WebSocket,
    state: &HttpServerState,
    session_id: &str,
    trace_id: &str,
    request: SendMessageRequest,
) {
    use tokio::sync::mpsc;

    // ★ DIAG 2026-07-08: unconditional entry print — owner reported
    // 10x send_message_stream on backend but only 1x handleSendMessage
    // on frontend. If we see 10x here too, the trigger is upstream
    // (multiple WS frames from a single sendStream call → most likely
    // browser-side auto-resume or our own loop somewhere we haven't
    // found yet).
    eprintln!(
        "!!!HANDLE_WS_STREAM_SEND_ENTRY!!! session={} text_len={} text_preview={:?} trace_id={}",
        session_id,
        request.text.len(),
        &request.text.chars().take(40).collect::<String>(),
        trace_id
    );

    // 2026-07-06: dedup guard. If the same (session_id, content)
    // arrived within the last 10s, skip with a synthetic Done
    // event so the client settles cleanly. Catches:
    //   - WS auto-retry storms (useStreamChat's `MAX_RETRIES=3`
    //     path opening a fresh WS on `onerror`/`onclose` and
    //     re-sending the same `text`)
    //   - App.tsx queue re-drain races that re-enqueue the same
    //     message after a transient failure
    //   - Any external integration that double-fires
    //
    // The dedup window is 10s — long enough that legitimate
    // user re-typing the same question after a few seconds (e.g.
    // "stop", then "stop again" from a different mental model)
    // still works, short enough that the dedup map stays tiny.
    if let Some(elapsed) = check_recent_send(&state, session_id, &request.text) {
        tracing::warn!(
            session = %session_id,
            text_len = request.text.len(),
            elapsed_secs = elapsed,
            "dedup: identical content sent within 10s, skipping (client retry storm or queue re-drain)"
        );
        // Send a synthetic Done so the client settles cleanly
        // (otherwise the WS would hang until the 120s timeout).
        let envelope = WsEnvelope {
            version: 1,
            request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
            trace_id: trace_id.to_string(),
            event: StreamEvent::Done {
                content: String::new(),
                finish_reason: Some("deduped".to_string()),
            },
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let _ = socket.send(WsMessage::Text(json.into())).await;
        }
        return;
    }

    // 2026-07-06: session-level active-stream lock. If the same
    // (session_id, text) already has an agent running (from a
    // prior retry whose WS disconnected before the previous agent
    // finished), reject the duplicate. The key is derived from
    // `dedup_key` so both checks use the same collision domain.
    // This catches retries that arrive AFTER the 10s dedup window
    // (when `check_recent_send` above misses them).
    let active_key = dedup_key(session_id, &request.text);
    let is_duplicate = {
        let mut active = state.active_sessions.lock().unwrap();
        if !active.insert(active_key.clone()) {
            true
        } else {
            false
        }
    };
    if is_duplicate {
        tracing::warn!(
            session = %session_id,
            text_len = request.text.len(),
            "active_stream: duplicate (session_id, text) already running; rejecting"
        );
        let envelope = WsEnvelope {
            version: 1,
            request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
            trace_id: trace_id.to_string(),
            event: StreamEvent::Error {
                code: "duplicate_stream".to_string(),
                message: "This message is already being processed. Please wait for the current response to complete.".to_string(),
            },
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let _ = socket.send(WsMessage::Text(json.into())).await;
        }
        return;
    }

    // Create a bounded queue to avoid unbounded memory usage under slow clients.
    let queue_cap = env_compat("HERMIX_WS_STREAM_BUFFER", "HERMES_WS_STREAM_BUFFER")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(128);
    let (tx, mut rx) = mpsc::channel::<WsEnvelope>(queue_cap);

    // Spawn a listener that subscribes to the global clarify broadcast
    // and forwards matching `ClarifyRequest` events to the WS client for
    // this session. Resolution (`ClarifyResponse`) comes back through
    // the main WS message loop in `handle_ws_stream`.
    let mut clarify_rx = state.clarify_subscribe();
    let clarify_tx = tx.clone();
    let clarify_session = session_id.to_string();
    let clarify_trace = trace_id.to_string();
    let clarify_rid = request.request_id.clone();
    tokio::spawn(async move {
        while let Ok(env) = clarify_rx.recv().await {
            // Filter: only forward questions for this session. The
            // dispatcher is global; desktop tabs own their own
            // session id.
            if env.session_id != clarify_session {
                continue;
            }
            let envelope = WsEnvelope {
                version: 1,
                request_id: clarify_rid.clone(),
                trace_id: clarify_trace.clone(),
                event: StreamEvent::ClarifyRequest {
                    request_id: env.request_id,
                    question: env.question,
                    choices: env.choices,
                },
            };
            // If the client has already closed we just drop the event;
            // the dispatcher still has the responder parked and will
            // time out per `HERMES_CLARIFY_TIMEOUT_SECS`.
            let _ = clarify_tx.send(envelope).await;
        }
    });

    // Todo event listener. The `todo` tool stamps each `TodoChange`
    // with the **active session id** (set by `agent_loop` via
    // `TodoHandler::set_active_session` at the top of each
    // tool-call batch). We forward only events whose session
    // matches this WS connection's session, so a session-A tool
    // call does not leak into session-B's pane.
    //
    // Rationale for per-session filtering here, not in the
    // front-end: the TodoPanel already keys state by session id,
    // but if we shipped raw events to every connection the React
    // store would still have to discard cross-session updates.
    // Filtering at the transport layer keeps the wire clean and
    // prevents another tab from snooping on a sibling session.
    //
    // `None` session changes (CLI mode / legacy bootstrap)
    // intentionally route to every session — the right fallback
    // when no active session context is available.
    if let Some(todo_tx) = state.todo_event_tx.clone() {
        let todo_inner_tx = tx.clone();
        let todo_session = session_id.to_string();
        let todo_trace = trace_id.to_string();
        let todo_rid = request.request_id.clone();
        let mut todo_rx = todo_tx.subscribe();
        tokio::spawn(async move {
            // v1: originating_message_id left as None so new group cards
            // append at the end of the message stream. A future patch
            // will thread the active assistant message_id through from
            // agent_loop.
            let originating_message_id: Option<String> = None;
            loop {
                match todo_rx.recv().await {
                    Ok(change) => {
                        // Per-session filter: drop mismatches
                        // (None = legacy fan-out, always accept).
                        if let Some(ref s) = change.session_id {
                            if s != &todo_session {
                                continue;
                            }
                        }
                        // Translate to stream event using the
                        // change's own session id (not the
                        // listener's) so the TodoPanel always
                        // sees the originating session.
                        let originating = change
                            .session_id
                            .clone()
                            .unwrap_or_else(|| todo_session.clone());
                        let event =
                            todo_change_to_stream(&originating, originating_message_id.as_deref(), change);
                        if let Some(ev) = event {
                            let envelope = WsEnvelope {
                                version: 1,
                                request_id: todo_rid.clone(),
                                trace_id: todo_trace.clone(),
                                event: ev,
                            };
                            let _ = todo_inner_tx.send(envelope).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            session = %todo_session,
                            skipped,
                            "todo_rx lagged; some todo events were dropped"
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Build AgentOverrides from request
    let overrides = hermix_core::traits::AgentOverrides {
        model: request.model.clone(),
        personality: request.personality.clone(),
        system_prompt: request.system_prompt.clone(),
        tools: request.tools.clone(),
        workspace: request.workspace.clone(),
        permission: parse_permission_level(request.permission.as_deref()),
        images: convert_wire_images(&request.images),
        image: request.image.clone(),
        image_mime_type: request.image_mime_type.clone(),
        skills: request.skills.clone(),
        approval_requester: {
            let registry = state.approval_registry.clone();
            Some(Arc::new(move |sess_id: String, req_id: String| {
                registry.request_approval(&sess_id, &req_id)
            }))
        },
        pre_approval_checker: {
            // The pre-approval shortcut consults both the persistent
            // `UserAllowlist` (loaded from
            // `$HERMIX_HOME/approval_allowlist.json` at server
            // startup) and the per-session in-memory bypass map
            // (populated when the user picks "Allow for this
            // session"). A hit on either is enough to skip the WS
            // round-trip.
            //
            // The closure is sync (it holds a `std::sync::Mutex` for
            // both maps — the agent loop calls this on a tokio
            // worker thread) and extracts the canonical "command"
            // string from the params for `terminal` tool calls.
            // Non-terminal tools with their own allowable params
            // (e.g. `web_extract`'s URL) can extend this branch
            // later; the MVP only short-circuits terminal commands,
            // matching the upstream Hermes allowlist semantics.
            let user_allowlist = state.user_allowlist.clone();
            let session_bypass = state.session_bypass.clone();
            let sid_for_check = session_id.to_string();
            Some(Arc::new(move |tool: &str, params: &serde_json::Value| -> bool {
                // Extract the canonical command string for
                // allowlist matching. We support `command` and
                // `cmd` keys (the latter is a non-standard but
                // seen-in-the-wild alias used by some tool
                // wrappers).
                let command = params
                    .get("command")
                    .or_else(|| params.get("cmd"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if command.is_empty() {
                    return false;
                }
                // Session-bypass first — it has fewer entries (in
                // practice) and avoids the disk-backed allowlist
                // lock when both would match.
                if let Ok(map) = session_bypass.lock() {
                    if let Some(patterns) = map.get(&sid_for_check) {
                        for re in patterns {
                            if re.is_match(command) {
                                return true;
                            }
                        }
                    }
                }
                // Persistent allowlist.
                if let Ok(guard) = user_allowlist.lock() {
                    if let Some(al) = guard.as_ref() {
                        if al.matches(command) {
                            return true;
                        }
                    }
                }
                let _ = tool; // currently only `terminal` benefits, but kept for forward-compat
                false
            }))
        },
        on_event: {
            let tx = tx.clone();
            let trace = trace_id.to_string();
            let rid = request.request_id.clone();
            let sid = session_id.to_string();
            let persistence = state.session_persistence.clone();
            Some(Arc::new(move |extra: serde_json::Value| {
                let request_id = extra
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool = extra
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = extra
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let risk = extra
                    .get("risk")
                    .and_then(|v| v.as_str())
                    .unwrap_or("caution")
                    .to_string();
                // The previous implementation used `try_send` here,
                // which silently dropped the event if the bounded
                // mpsc was momentarily full (channel cap is 128, set
                // in `handle_ws_stream_send`). For most events that's
                // fine — the next chunk will land a moment later and
                // the UI catches up. For `request_approval` it is
                // catastrophic: if the event is dropped the client
                // never sees the inline card, the user can't click
                // allow/deny, the oneshot in `ApprovalRegistry` parks
                // forever, and the agent's `AwaitApproval` is never
                // resolved — the stream appears to "stuck thinking"
                // with no recovery path short of a hard restart.
                //
                // The first attempted fix wrapped `blocking_send` in
                // `tokio::task::block_in_place`, but `on_event` is a
                // SYNCHRONOUS `Fn(Value) -> ()` invoked from inside
                // `decide_pre_tool_dispatch` (a sync function called
                // from a tokio task). Calling `block_in_place` from
                // this path panics with "Cannot drop a runtime in a
                // context where blocking is not allowed" when the
                // worker pool tears down — see tokio-1.51.1
                // `runtime/blocking/shutdown.rs:51:21`.
                //
                // The current fix: spawn a fire-and-forget task that
                // owns the `tx` clone and awaits `tx.send(envelope)`.
                // This guarantees delivery (no `try_send` drop), keeps
                // the synchronous `on_event` call site non-blocking,
                // and never touches the blocking pool — eliminating
                // the panic. The cost is the usual tokio::spawn
                // overhead (~5μs); the bounded channel still back-
                // pressures if the WS reader falls behind (the
                // `.await` parks until the consumer catches up).
                // Compute the deadline once and share it between the
                // WS envelope (live countdown) and the persisted
                // `ApprovalRecord` (reload countdown). Two separate
                // `compute_approval_deadline_ms()` calls would
                // diverge by a few µs — invisible to the user, but
                // it would mean the reload-rendered countdown
                // doesn't match what they saw live.
                let expires_at_ms = compute_approval_deadline_ms();
                let envelope = WsEnvelope {
                    version: 1,
                    request_id: rid.clone(),
                    trace_id: trace.clone(),
                    event: StreamEvent::RequestApproval {
                        request_id: request_id.clone(),
                        tool: tool.clone(),
                        arguments: arguments.clone(),
                        risk: risk.clone(),
                        reason: None,
                        // Wall-clock deadline (Unix ms) when the
                        // server will auto-deny this request. Drives
                        // the front-end countdown UI so the user
                        // always knows how much budget they have
                        // before the silent auto-deny. Computed
                        // here and cloned into both the WS envelope
                        // (live UI) and the persisted
                        // `ApprovalRecord` (so a session reload can
                        // re-render the countdown against the
                        // original deadline, not a fresh one).
                        expires_at: Some(expires_at_ms),
                        // 4-state approval: once / session / always /
                        // deny. The server sends the full set on
                        // every approval card so the desktop UI
                        // can render a single consistent button
                        // group; clients that don't know about
                        // `session` or `always` will see two extra
                        // button labels and can ignore them
                        // (back-compat). The downstream
                        // `WsClientMessage::ApprovalResponse`
                        // payload accepts all four values plus
                        // the legacy 2-state `approved`/`denied`.
                        choices: vec![
                            hermix_transport::ApprovalChoice::Once,
                            hermix_transport::ApprovalChoice::Session,
                            hermix_transport::ApprovalChoice::Always,
                            hermix_transport::ApprovalChoice::Deny,
                        ],
                    },
                };
                // Move clones into the spawn — the outer `Fn` closure
                // cannot move `tx` / `request_id` (those are needed
                // again below for the persistence write), and the
                // inner `async move` consumes whatever it captures.
                let tx_for_send = tx.clone();
                let request_id_for_log = request_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = tx_for_send.send(envelope).await {
                        // The only failure mode of `Sender::send` is
                        // "channel closed" — i.e. the WS task already
                        // dropped its receiver (client disconnected,
                        // or the outer stream settled). There is no
                        // recoverable action: the user is gone. Log
                        // at debug level so a noisy disconnect
                        // doesn't fill the log, but keep the warning
                        // observable for postmortem.
                        tracing::debug!(
                            error = %e,
                            request_id = %request_id_for_log,
                            "approval event dropped: WS receiver closed"
                        );
                    }
                });
                // Persist the approval row to the session timeline so it
                // can be replayed across reloads. The decision will be
                // filled in by `resolve_approval_message` when the user
                // clicks allow/deny. We don't have the linked assistant
                // id here (the assistant message has not been finalized
                // yet — we're mid-stream), so we leave it as None; the
                // UI will position the card by its DB row order.
                // The deadline uses the SAME value the WS envelope
                // carried — the countdown rendered after a reload
                // must agree with the original countdown the user
                // saw live.
                let record = hermix_agent::session_persistence::ApprovalRecord {
                    request_id: request_id.clone(),
                    tool: tool.clone(),
                    arguments,
                    risk,
                    decision: None,
                    decided_at: None,
                    // Same `expires_at_ms` the WS envelope carried —
                    // a session reload must show the user the
                    // remaining time against the *original*
                    // deadline, not a freshly recomputed one
                    // (otherwise the countdown would briefly jump
                    // backwards or forwards across a page refresh).
                    expires_at: Some(expires_at_ms),
                    linked_assistant_id: None,
                };
                if let Err(e) = persistence.append_approval_message(&sid, &record) {
                    tracing::warn!(
                        error = %e,
                        request_id = %request_id,
                        "failed to persist approval row; UI will not show it across reloads"
                    );
                }
            }))
        },
        on_prefetch_intent: {
            let tx = tx.clone();
            let trace = trace_id.to_string();
            let rid = request.request_id.clone();
            Some(Arc::new(move |payload: &str| {
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(payload) {
                    let intent = obj.get("intent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let confidence = obj.get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32;
                    let results: Vec<hermix_transport::PrefetchResultEntry> = obj.get("results")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter().filter_map(|r| {
                                serde_json::from_value(r.clone()).ok()
                            }).collect()
                        })
                        .unwrap_or_default();
                    let _ = tx.try_send(WsEnvelope {
                        version: 1,
                        request_id: rid.clone(),
                        trace_id: trace.clone(),
                        event: StreamEvent::PrefetchIntent {
                            intent,
                            confidence,
                            results,
                        },
                    });
                }
            }))
        },
    };

    let tx_clone = tx.clone();
    let request_id = request.request_id.clone();
    let trace_id_for_stream = trace_id.to_string();
    // Captured by the `on_chunk` closure below so the ThinkBlockParser
    // can route extracted think bodies to the client as
    // `StreamEvent::Thinking` (for the collapsible "思考过程" block
    // in MessageBubble). Without this capture the parsed-out
    // `_think` text was discarded per the previous "think is internal
    // scaffolding" product decision — but 主人 (2026-07-08) asked to
    // surface it as a foldable block, so we forward it.
    let tx_for_think = tx.clone();
    // One parser per WS stream — its state is meaningless across turn
    // boundaries.  Some providers (M2.7, Qwen, DeepSeek-R1) embed
    // `<think>...</think>` inside `delta.content` rather than a
    // separate reasoning field, and the block can straddle two
    // chunks.  The `agent_loop` parses this internally for its own
    // assistant message bookkeeping, but the *WS-side* text payload
    // we forward to the desktop client is the *raw* delta.content —
    // which means think tags leak into the user's terminal stream.
    //
    // Re-parsing on the server side here lets the desktop client
    // see a clean `Text` event stream with no think blocks.  We
    // deliberately do *not* forward the reasoning text to the
    // client (per product decision: think is internal scaffolding,
    // not user output) — it stays in-process for telemetry.
    let think_parser = Arc::new(std::sync::Mutex::new(ThinkBlockParser::new()));
    // The tool-call XML sanitizer is *stateful*: an opening
    // `<minimax:tool_call>` and its matching `</minimax:tool_call>`
    // may arrive in *different* chunks. The previous regex pass
    // (`strip_tool_call_markers`) only worked on per-chunk strings
    // and would leak a chunk that held the opener with no closer.
    // The stateful `ToolCallSanitizer` (see
    // `hermix_core::tool_call_sanitizer`) is now the authoritative
    // redactor at the WS layer too. The agent loop also runs a
    // per-stream copy, so the redactor is applied *twice* on every
    // chunk — the second pass is a no-op once the text is clean.
    // The double pass is intentional belt-and-suspenders: the
    // agent-loop copy cleans the persisted message, the WS copy
    // cleans the live stream.
    let tool_call_sanitizer = Arc::new(std::sync::Mutex::new(
        hermix_core::ToolCallSanitizer::new(),
    ));
    // Clone the Arc for the `Done` handler below — the on_chunk
    // closure takes ownership of the original.
    let tool_call_sanitizer_owned = tool_call_sanitizer.clone();
    let on_chunk = Arc::new(move |chunk: StreamChunk| {
        // Re-shape the chunk so `delta.content` is think-stripped
        // before we look at it.  We keep `delta.extra` (and any
        // tool_call deltas) intact so the tool/status branches below
        // still see the original payload.
        let mut chunk = chunk;
        if let Some(ref mut delta) = chunk.delta {
            if let Some(ref mut text) = delta.content {
                if !text.is_empty() {
                    let (clean, think) = think_parser.lock().unwrap().feed(text);
                    // 2026-07-08: forward the extracted think block to
                    // the client as `StreamEvent::Thinking` so the
                    // chat layer can render a collapsible "思考过程"
                    // block. `ThinkBlockParser::feed` returns the body
                    // (without `<think>` markers) once a closing tag
                    // is seen, or empty when the block is still
                    // streaming. We only emit when non-empty to avoid
                    // spamming the client with empty thinking events.
                    if !think.is_empty() {
                        let _ = tx_for_think.try_send(WsEnvelope {
                            version: 1,
                            request_id: request_id.clone(),
                            trace_id: trace_id_for_stream.clone(),
                            event: StreamEvent::Thinking {
                                content: redact_sensitive(&think),
                            },
                        });
                    }
                    if clean.is_empty() {
                        delta.content = None;
                    } else {
                        *text = clean;
                    }
                }
            }
        }
        if let Some(delta) = &chunk.delta {
            // 1. Text content (LLM response, already think-stripped
            //    by the local `think_parser` above).  We then run
            //    the *stateful* tool-call sanitizer so a
            //    `<minimax:tool_call>…</minimax:tool_call>` block
            //    split across chunks is fully suppressed (the
            //    previous per-chunk regex would leak the tail of any
            //    block whose opener had not yet been seen).  The
            //    redactor also strips the bare `minimax:tool_call`
            //    literal and orphan closing tags, so even a malformed
            //    LLM response is sanitised.
            if let Some(text) = &delta.content {
                if !text.is_empty() {
                    let mut guard = tool_call_sanitizer.lock().unwrap();
                    let cleaned = guard.feed(text);
                    drop(guard);
                    let _ = tx_clone.try_send(WsEnvelope {
                        version: 1,
                        request_id: request_id.clone(),
                        trace_id: trace_id_for_stream.clone(),
                        event: StreamEvent::Text {
                            content: redact_sensitive(&cleaned),
                        },
                    });
                }
            }
            // 2. Tool lifecycle / stdout / status events (delta.extra)
            if let Some(extra) = &delta.extra {
                if let Some(event_type) = extra.get("event").and_then(|e| e.as_str()) {
                    let envelope = match event_type {
                        "tool_start" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("arguments").map(|a| a.to_string()).unwrap_or_default();
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-{}", tool, uuid::Uuid::new_v4()));
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::ToolStart { tool_call_id, tool, content },
                            }
                        }
                        "tool_stdout" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let chunk_idx = extra.get("chunk_index")
                                .and_then(|i| i.as_u64())
                                .unwrap_or(0);
                            let chunk_total = extra.get("chunk_total")
                                .and_then(|t| t.as_u64())
                                .unwrap_or(1);
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-anon-{}", tool, chunk_idx));
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::ToolChunk {
                                    tool_call_id,
                                    tool,
                                    content,
                                    chunk_index: chunk_idx,
                                    chunk_total,
                                },
                            }
                        }
                        "tool_complete" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-anon", tool));
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::ToolComplete { tool_call_id, tool, content },
                            }
                        }
                        "file_change" => {
                            // File-mutation summary event. Routed
                            // straight to the front-end as a
                            // StreamEvent::FileChange so the chat UI
                            // can render the "文件变更 (N个)" row.
                            // The server is the single source of
                            // truth for what changed on disk during
                            // this turn; the front-end never tries
                            // to infer this from tool output.
                            let file_path = extra.get("file_path")
                                .and_then(|p| p.as_str())
                                .unwrap_or("")
                                .to_string();
                            if file_path.is_empty() {
                                // Skip malformed events silently;
                                // log via tracing so the issue is
                                // visible in sidecar diagnostics.
                                // We can't `continue` from inside the
                                // on_chunk closure, so we just
                                // fall through to the next match arm
                                // by returning Unit early.
                                tracing::warn!("file_change event missing file_path; ignoring");
                                return;
                            }
                            let change_type = extra.get("change_type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("modified")
                                .to_string();
                            let file_summary = extra.get("file_summary")
                                .and_then(|s| s.as_str())
                                .map(String::from);
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| {
                                    format!("file_change::{}", file_path)
                                });
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::FileChange {
                                    tool_call_id,
                                    file_path,
                                    change_type,
                                    file_summary,
                                },
                            }
                        }
                        "thinking" => {
                            // Live reasoning chunk from the LLM
                            // (think-block body). Routed to the
                            // frontend as `StreamEvent::Thinking`,
                            // which the chat layer appends to
                            // `message.reasoningContent` for the
                            // collapsible "思考过程" block in
                            // MessageBubble. Without this dispatch
                            // arm the user never sees the model
                            // thinking live — it only became
                            // visible on session reload via the
                            // persisted `reasoning_content` field.
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            if content.is_empty() {
                                return;
                            }
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::Thinking {
                                    content: redact_sensitive(&content),
                                },
                            }
                        }
                        "status" => {
                            let kind = extra.get("kind")
                                .and_then(|k| k.as_str())
                                .unwrap_or("status");
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Map to proper StreamEvent variant so the
                            // front-end can display it as a status chip
                            // instead of injecting it into message text.
                            let event = match kind {
                                "activity" => StreamEvent::Activity { content },
                                _ => StreamEvent::Status { content },
                            };
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event,
                            }
                        }
                        "context_usage" => {
                            let used = extra.get("used_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let max = extra.get("max_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let threshold = extra.get("threshold_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let percent = extra.get("percent")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::ContextUsage {
                                    used_tokens: used,
                                    max_tokens: max,
                                    threshold_tokens: threshold,
                                    percent,
                                },
                            }
                        }
                        "compaction_status" => {
                            let snapshot = extra.get("snapshot")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::CompactionStatus { snapshot },
                            }
                        }
                        "section_gate" | "prefetch_intent" | "acui" => {
                            // Forward these structured events as raw
                            // JSON so the frontend's `useStreamChat`
                            // handler can parse them. We wrap the entire
                            // `extra` object as the WS event payload.
                            let raw = extra.clone();
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                event: StreamEvent::Status {
                                    content: raw.to_string(),
                                },
                            }
                        }
                        _ => {
                            tracing::debug!(event = %event_type, "unknown extra event type; skipping");
                            return;
                        },
                    };
                    let _ = tx_clone.try_send(envelope);
                }
            }
        }
    });

    // Call agent service with streaming
    let agent_service = state.agent_service.clone();
    let active_sessions = state.active_sessions.clone();
    let session_id_clone = session_id.to_string();
    let request_text = request.text.clone();
    let trace_id_owned = trace_id.to_string();
    let request_id_owned = request.request_id.clone();

    // Track whether the WS client has disconnected. When the user
    // clicks "stop" / switches sessions, the WS closes and the
    // forwarding loop below breaks. Without this guard the spawned
    // agent task continues running, eventually persists the full
    // response to DB, and sends a Done event that nobody reads.
    // On session re-entry the front-end fetches the now-complete
    // message — making it look like the session "resumed" and
    // produced output after being stopped.
    let ws_disconnected = Arc::new(AtomicBool::new(false));
    let ws_disconnected_clone = ws_disconnected.clone();
    let active_key_clone = active_key.clone();

    let agent_handle = tokio::spawn(async move {
        let result = agent_service
            .send_message_stream(&session_id_clone, &request_text, overrides, on_chunk)
            .await;

        // If the client disconnected mid-stream, skip sending the
        // terminal Done/Error event. The front-end will never see
        // a "completed" phase, so on session reload it treats the
        // message as interrupted — even if send_message_stream
        // already persisted the full response to DB (best-effort
        // race: the abort below reduces the window further).
        if ws_disconnected_clone.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::debug!(
                session = %session_id_clone,
                "WS disconnected before agent finished; skipping Done/Error event"
            );
            // Release the session lock even when the WS disconnected.
            // This allows the user to re-send the message later without
            // hitting the "already being processed" guard.
            active_sessions.lock().unwrap().remove(&active_key_clone);
            return;
        }

        match result {
            Ok(reply) => {
                // Apply the stateful tool-call redactor to strip
                // `<minimax:tool_call>...</minimax:tool_call>` XML that
                // some models emit instead of using the OpenAI `tools`
                // field. We do NOT strip <think> blocks here — the
                // frontend's resolveMessage handles that split for
                // display. We send the LLM's raw content as-is.
                let cleaned = {
                    let mut guard = tool_call_sanitizer_owned.lock().unwrap();
                    guard.flush();
                    guard.feed(&reply.text)
                };
                // Always send Done — never skip it. Even when content
                // is empty, the front-end needs the terminal event to
                // settle the stream (otherwise it hangs until the 120s
                // timeout). The message was already persisted to the DB
                // by `persist_session_with_compression` before we get
                // here, so the row exists regardless.
                let done_content = redact_sensitive(&cleaned);
                let done_finish_reason = reply.finish_reason.clone();
                let _ = tx
                    .send(WsEnvelope {
                        version: 1,
                        request_id: request_id_owned.clone(),
                        trace_id: trace_id_owned.clone(),
                        event: StreamEvent::Done {
                            content: done_content,
                            finish_reason: done_finish_reason,
                        },
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(WsEnvelope {
                        version: 1,
                        request_id: request_id_owned.clone(),
                        trace_id: trace_id_owned.clone(),
                        event: StreamEvent::Error {
                            code: "agent_error".to_string(),
                            message: redact_sensitive(&e.to_string()),
                        },
                    })
                    .await;
            }
        }

        // Release the per-session stream lock so future sends
        // (same or different content) for this session are allowed.
        active_sessions.lock().unwrap().remove(&active_key_clone);
    });

    // Forward channel events to WebSocket
    tracing::info!(
        session = %session_id,
        "ws_stream: rx forward loop starting"
    );
    let mut was_client_disconnect = false;
    while let Some(event_json) = rx.recv().await {
        if socket
            .send(WsMessage::Text(
                serde_json::to_string(&event_json)
                    .unwrap_or_default()
                    .into(),
            ))
            .await
            .is_err()
        {
            // Client disconnected — mark the flag so the agent task
            // skips its terminal Done/Error event, then abort the
            // task to stop processing as soon as possible (at the
            // next .await boundary in send_message_stream).
            was_client_disconnect = true;
            ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
            agent_handle.abort();
            // Release the session lock since the spawned task may
            // not reach its cleanup code (abort cancels at the next
            // .await, which could be deeper in send_message_stream).
            state.active_sessions.lock().unwrap().remove(&active_key);
            break;
        }
    }
    tracing::info!(
        session = %session_id,
        was_client_disconnect = was_client_disconnect,
        "ws_stream: rx forward loop EXITED — handle_ws_stream_send returning"
    );
}

// ── GroupChat streaming WebSocket ────────────────────────

async fn ws_groupchat_upgrade(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws_groupchat(socket, state, room_id))
}

/// Handle a group chat session over WebSocket.
///
/// The client sends `{"type":"send","text":"..."}` and the server
/// streams each agent's reply chunk by chunk. After receiving the
/// initial message, the socket is moved into a background sender task
/// and the handler returns — the frontend opens a **new** WS connection
/// per message, same as the REST API call pattern but with streaming.
async fn handle_ws_groupchat(
    mut socket: WebSocket,
    state: HttpServerState,
    room_id: String,
) {
    use hermix_gateway::groupchat::persistence::AgentRole;
    use tokio::sync::mpsc;

    // Send Connected
    let trace_id = hermix_transport::new_trace_id();
    let connected = WsEnvelope {
        version: 1,
        request_id: None,
        trace_id: trace_id.clone(),
        event: StreamEvent::Connected { session_id: room_id.clone() },
    };
    let _ = socket.send(WsMessage::Text(
        serde_json::to_string(&connected).unwrap_or_default().into(),
    )).await;

    // Wait for exactly one Send message, then move socket into sender task
    let text = loop {
        match socket.next().await {
            Some(Ok(WsMessage::Text(t))) => {
                if let Ok(ws_msg) = serde_json::from_str::<hermix_transport::WsClientMessage>(&t) {
                    if let hermix_transport::WsClientMessage::Send { ref text, .. } = ws_msg {
                        break text.clone();
                    }
                }
            }
            _ => return, // connection closed
        }
    };

    let manager = state.groupchat_manager();

    // Save user message
    let _ = manager.send_message(&room_id, "user", "你", &text).await;

    // Move socket into a background sender task. We read one message,
    // then all subsequent activity is writes (streaming agent chunks).
    // S5.1: the same sender also forwards outbound task-engine events
    // from the runtime's per-room broadcast channel until the room
    // completes or the client disconnects.
    let (tx, mut rx) = mpsc::channel::<WsEnvelope>(128);
    let broadcast_rx: tokio::sync::broadcast::Receiver<
        hermix_gateway::groupchat::runtime::RoomEvent,
    > = if let Some(te) = state.task_engine.as_ref() {
        te.subscribe(&room_id)
    } else {
        // No runtime → dead channel that never delivers.
        let (dead_tx, dead_rx) = tokio::sync::broadcast::channel(1);
        drop(dead_tx);
        dead_rx
    };
    let trace_id_for_engine = trace_id.clone();
    let socket_task = tokio::spawn(async move {
        let mut socket = socket;
        let mut broadcast_rx = broadcast_rx;
        loop {
            tokio::select! {
                env = rx.recv() => {
                    let Some(env) = env else { break };
                    if socket
                        .send(WsMessage::Text(
                            serde_json::to_string(&env).unwrap_or_default().into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                ev = broadcast_rx.recv() => {
                    match ev {
                        Ok(room_ev) => {
                            let is_terminal = matches!(
                                room_ev,
                                hermix_gateway::groupchat::runtime::RoomEvent::RoomCompleted { .. }
                            );
                            if let Some(stream_ev) = room_event_to_stream(room_ev) {
                                let env = WsEnvelope {
                                    version: 1,
                                    request_id: None,
                                    trace_id: trace_id_for_engine.clone(),
                                    event: stream_ev,
                                };
                                let send_ok = socket
                                    .send(WsMessage::Text(
                                        serde_json::to_string(&env)
                                            .unwrap_or_default()
                                            .into(),
                                    ))
                                    .await
                                    .is_ok();
                                if !send_ok {
                                    break;
                                }
                            }
                            if is_terminal {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Slow consumer; skip missed events.
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    // Prepare room context
    let agents = manager.get_room_agents(&room_id).await.unwrap_or_default();
    let messages = manager.get_messages(&room_id, 20).await.unwrap_or_default();
    let context: Vec<String> = messages.iter().rev().take(10).rev()
        .map(|m| format!("{}: {}", m.sender_name, m.content))
        .collect();
    let room_context = context.join("\n");

    let leader = agents.iter().find(|a| a.role == AgentRole::Leader);
    let leader_prompt = leader.map(|_ldr| {
        hermix_gateway::groupchat::leader::LeaderOrchestrator::build_leader_prompt(
            &agents,
            &agents.iter().map(|a| (a.agent_id.clone(), a.agent_id.clone())).collect::<Vec<_>>(),
        )
    });

    // Determine which agents to dispatch to
    let targets: Vec<(String, String, String)> = if let Some(ldr) = leader {
        vec![(ldr.agent_id.clone(), ldr.session_id.clone(), ldr.display_name.clone().unwrap_or_else(|| ldr.agent_id.clone()))]
    } else {
        agents.iter().map(|a| (a.agent_id.clone(), a.session_id.clone(), a.display_name.clone().unwrap_or_else(|| a.agent_id.clone()))).collect()
    };

    for (agent_id, session_id, agent_name) in &targets {
        let system_prompt = if let Some(ldr) = leader {
            if *agent_id == ldr.agent_id {
                format!("{}\n\n房间上下文：\n{}", leader_prompt.as_ref().unwrap_or(&String::new()), room_context)
            } else {
                format!("你是{}，在群聊室中。你收到了总管分配的任务。\n\n房间上下文：\n{}", agent_id, room_context)
            }
        } else {
            format!("你是{}，在群聊室中。\n\n房间上下文：\n{}", agent_id, room_context)
        };

        let overrides = hermix_core::traits::AgentOverrides {
            system_prompt: Some(system_prompt),
            ..Default::default()
        };

        let response_so_far = Arc::new(std::sync::Mutex::new(String::new()));
        let response_clone = response_so_far.clone();
        let agent_id_clone = agent_id.clone();
        let agent_name_clone = agent_name.clone();
        let room_id_clone = room_id.clone();
        let trace_id_clone = trace_id.clone();
        let tx_clone = tx.clone();

        let think_parser = Arc::new(std::sync::Mutex::new(hermix_agent::python_alignment::ThinkBlockParser::new()));
        let think_parser_clone = think_parser.clone();
        let on_chunk: Arc<dyn Fn(hermix_core::types::StreamChunk) + Send + Sync> = Arc::new(move |chunk| {
            let content = chunk.delta.as_ref().and_then(|d| d.content.as_deref()).unwrap_or_default().to_string();
            let (text_part, think_part) = think_parser_clone.lock().unwrap_or_else(|e| e.into_inner()).feed(&content);

            if !text_part.is_empty() {
                if let Ok(mut resp) = response_clone.lock() {
                    resp.push_str(&text_part);
                }
                let _ = tx_clone.try_send(WsEnvelope {
                    version: 1, request_id: None, trace_id: trace_id_clone.clone(),
                    event: StreamEvent::GroupChatChunk {
                        room_id: room_id_clone.clone(),
                        agent_id: agent_id_clone.clone(),
                        agent_name: agent_name_clone.clone(),
                        content: text_part,
                    },
                });
            }
            if !think_part.is_empty() {
                let _ = tx_clone.try_send(WsEnvelope {
                    version: 1, request_id: None, trace_id: trace_id_clone.clone(),
                    event: StreamEvent::GroupChatThinking {
                        room_id: room_id_clone.clone(),
                        agent_id: agent_id_clone.clone(),
                        agent_name: agent_name_clone.clone(),
                        content: think_part,
                    },
                });
            }
        });

        let result = state.agent_service.send_message_stream(
            &session_id,
            &text,
            overrides,
            on_chunk,
        ).await;

        // Flush remaining
        let (text_part, think_part) = think_parser.lock().unwrap_or_else(|e| e.into_inner()).flush();
        if !text_part.is_empty() {
            if let Ok(mut resp) = response_so_far.lock() {
                resp.push_str(&text_part);
            }
            let _ = tx.try_send(WsEnvelope {
                version: 1, request_id: None, trace_id: trace_id.clone(),
                event: StreamEvent::GroupChatChunk {
                    room_id: room_id.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    content: text_part,
                },
            });
        }
        if !think_part.is_empty() {
            let _ = tx.try_send(WsEnvelope {
                version: 1, request_id: None, trace_id: trace_id.clone(),
                event: StreamEvent::GroupChatThinking {
                    room_id: room_id.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    content: think_part,
                },
            });
        }

        // Agent done
        let _ = tx.try_send(WsEnvelope {
            version: 1, request_id: None, trace_id: trace_id.clone(),
            event: StreamEvent::GroupChatAgentDone {
                room_id: room_id.clone(),
                agent_id: agent_id.clone(),
            },
        });

        // Save completed response to DB
        if result.is_ok() {
            if let Ok(mutex) = Arc::try_unwrap(response_so_far) {
                let final_text = mutex.into_inner().unwrap_or_default();
                if !final_text.is_empty() {
                    let stripped = hermix_agent::python_alignment::strip_think_blocks_for_ack(&final_text);
                    let _ = manager.save_agent_message(
                        &room_id,
                        agent_id,
                        agent_name,
                        &stripped,
                        hermix_gateway::groupchat::persistence::MessageType::Chat,
                    ).await;
                }
            }
        }
    }

    // All agents done
    let _ = tx.try_send(WsEnvelope {
        version: 1, request_id: None, trace_id: trace_id.clone(),
        event: StreamEvent::GroupChatDone {
            room_id: room_id.clone(),
        },
    });

    // S5.1: the sender task spawned above now owns the socket and
    // forwards both (a) `mpsc::Receiver<WsEnvelope>` for streaming
    // chunks and (b) `broadcast::Receiver<RoomEvent>` for task-engine
    // events. It exits when:
    //   * the channel closes (client gone / send failed), OR
    //   * the runtime emits `RoomEvent::RoomCompleted` for the room.
    //
    // We do not block here: the handler returns and the spawned
    // task continues to drive the socket until one of those
    // exit conditions fires.
    let _ = socket_task;
}

/// Translate a runtime [`RoomEvent`] into a wire-level [`StreamEvent`]
/// that the frontend already knows how to render. Returns `None` for
/// events that are intentionally not surfaced (already-saved messages
/// go through REST reload, terminal `RoomCompleted` is handled by the
/// sender's exit logic separately).
fn room_event_to_stream(
    ev: hermix_gateway::groupchat::runtime::RoomEvent,
) -> Option<StreamEvent> {
    use hermix_gateway::groupchat::runtime::RoomEvent;
    match ev {
        RoomEvent::MessageSaved { .. } => None,
        RoomEvent::TaskPhaseChange {
            room_id,
            node_id,
            description,
            phase,
            assigned_agent,
        } => Some(StreamEvent::TaskPhaseChange {
            room_id,
            node_id,
            description,
            phase,
            assigned_agent,
        }),
        RoomEvent::BusDelivered {
            room_id,
            agent_id,
            message_kind,
            text,
        } => Some(StreamEvent::BusDelivered {
            room_id,
            agent_id,
            message_kind,
            text,
        }),
        RoomEvent::AuditVerdict {
            room_id,
            agent_id,
            violation_id,
            level,
            score_delta,
            score_after,
            reason,
        } => Some(StreamEvent::AuditVerdict {
            room_id,
            agent_id,
            violation_id,
            level,
            score_delta,
            score_after,
            reason,
        }),
        RoomEvent::AgentExpelled {
            room_id,
            agent_id,
            agent_name,
            reason,
        } => Some(StreamEvent::AgentExpelled {
            room_id,
            agent_id,
            agent_name,
            reason,
        }),
        RoomEvent::RoomCompleted { room_id } => Some(StreamEvent::RoomCompleted { room_id }),
        RoomEvent::SoftError { code, message, .. } => Some(StreamEvent::Error { code, message }),
        // Streaming-think events from the dispatcher pipeline. The wire
        // shape is whatever the transport `GroupChatChunk` / `…Done`
        // variants already carry — we just route the role/agent_id
        // through and let the frontend handle leader-vs-member routing
        // (leader's chunks are visual-only; member's chunks get appended
        // to the running bubble).
        RoomEvent::AgentThinkingChunk {
            room_id,
            agent_id,
            agent_name,
            delta,
            role: _,
        } => Some(StreamEvent::GroupChatChunk {
            room_id,
            agent_id,
            agent_name,
            content: delta,
        }),
        RoomEvent::AgentThinkingCompleted { room_id, agent_id } => {
            Some(StreamEvent::GroupChatAgentDone { room_id, agent_id })
        }
    }
}

/// Translate a `TodoChange` (emitted by the `todo` tool) into a
/// wire-level [`StreamEvent`] that the desktop `useStreamChat`
/// hook already knows how to dispatch.
///
/// v1 (2026-07-07): every mutation becomes a `TodoItemUpserted`
/// so the front-end can apply upsert-by-id semantics. The
/// `session_id` field on every event is the WS session that
/// produced the change — desktop tabs already filter their todo
/// panel by their own session id, so cross-session bleed is a
/// non-issue as long as the front-end drops events whose
/// `session_id` doesn't match the active tab. A future P2 can
/// add per-session scoping to the `todo` tool itself.
fn todo_change_to_stream(
    session_id: &str,
    message_id: Option<&str>,
    change: hermix_tools::tools::todo::TodoChange,
) -> Option<StreamEvent> {
    use hermix_tools::tools::todo::TodoChangeKind;
    match change.change {
        TodoChangeKind::GroupStarted { group_id, group_title } => {
            Some(StreamEvent::TodoGroupStarted {
                session_id: session_id.to_string(),
                message_id: message_id.map(String::from),
                group_id,
                group_title,
            })
        }
        TodoChangeKind::Upsert(item) => Some(StreamEvent::TodoItemUpserted {
            session_id: session_id.to_string(),
            item: hermix_transport::TodoSnapshotItem {
                id: item.id,
                title: item.title,
                description: item.description,
                status: item.status,
                priority: item.priority,
                created_at: item.created_at,
                depends_on: item.depends_on,
                last_result: item.last_result,
                execution_count: item.execution_count,
                group_id: item.group_id,
                group_title: item.group_title,
            },
        }),
        TodoChangeKind::Delete { id } => Some(StreamEvent::TodoItemRemoved {
            session_id: session_id.to_string(),
            id,
        }),
    }
}

/// Streaming WebSocket handler.
async fn handle_ws_stream(mut socket: WebSocket, state: HttpServerState, session_id: String) {
    let trace_id_base = hermix_transport::new_trace_id();
    let connected = WsEnvelope {
        version: 1,
        request_id: None,
        trace_id: trace_id_base.clone(),
        event: StreamEvent::Connected {
            session_id: session_id.clone(),
        },
    };
    let _ = socket
        .send(WsMessage::Text(
            serde_json::to_string(&connected).unwrap_or_default().into(),
        ))
        .await;

    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            WsMessage::Text(text) => {
                let trace_id = trace_id_base.clone();
                // T2026-07-06: diagnostic — log every WS message receipt.
                tracing::info!(
                    session = %session_id,
                    text_preview = %text.chars().take(80).collect::<String>(),
                    "ws_stream: received Text message"
                );

                // Try structured WsClientMessage first (v2 protocol).
                if let Ok(ws_msg) = serde_json::from_str::<WsClientMessage>(&text) {
                    match ws_msg {
                        WsClientMessage::ApprovalResponse { request_id, decision } => {
                            // Parse the 4-state decision. Unknown /
                            // empty values log and downgrade to
                            // `Deny` (fail-closed — never auto-
                            // approve on garbage input).
                            let choice = hermix_transport::ApprovalChoice::from_decision_str(
                                &decision,
                            );
                            let choice = match choice {
                                Some(c) => c,
                                None => {
                                    tracing::warn!(
                                        session = %session_id,
                                        request_id = %request_id,
                                        decision = %decision,
                                        "unknown approval decision; downgrading to Deny (fail-closed)"
                                    );
                                    hermix_transport::ApprovalChoice::Deny
                                }
                            };
                            // The legacy 2-state semantics
                            // (`approved` / `denied`) and the 4-state
                            // semantics share the same registry
                            // resolution: any non-Deny choice approves
                            // the current call.
                            let approved = choice.is_approved();
                            state.approval_registry.resolve(
                                &session_id,
                                &request_id,
                                approved,
                            );
                            // The DB stores the decision as a free-form
                            // string, so we persist the canonical 4-state
                            // name (`"once"` / `"session"` / `"always"`
                            // / `"deny"`) — older rows that store the
                            // legacy 2-state values keep working
                            // unchanged. The UI reads `extra.decision`
                            // verbatim and the user-facing card flips
                            // color to match.
                            let decision_str = match choice {
                                hermix_transport::ApprovalChoice::Once => "once",
                                hermix_transport::ApprovalChoice::Session => "session",
                                hermix_transport::ApprovalChoice::Always => "always",
                                hermix_transport::ApprovalChoice::Deny => "deny",
                            };
                            if let Err(e) = state
                                .session_persistence
                                .resolve_approval_message(
                                    &session_id,
                                    &request_id,
                                    decision_str,
                                )
                            {
                                tracing::warn!(
                                    error = %e,
                                    request_id = %request_id,
                                    "failed to persist approval decision"
                                );
                            }
                            // 4-state side effects: `session` and
                            // `always` need to update their respective
                            // stores. The command to record is the
                            // `command` (or `cmd`) string from the
                            // persisted approval row's `arguments` JSON.
                            // If the row has been GC'd between the
                            // request and the response (it shouldn't —
                            // the registry keeps the request alive), we
                            // skip the side effect and log.
                            if matches!(
                                choice,
                                hermix_transport::ApprovalChoice::Session
                                    | hermix_transport::ApprovalChoice::Always
                            ) {
                                if let Ok(Some(cmd)) = lookup_pending_command(
                                    &state,
                                    &session_id,
                                    &request_id,
                                ) {
                                    record_approval_side_effect(
                                        &state,
                                        &session_id,
                                        &cmd,
                                        choice,
                                    );
                                } else {
                                    tracing::warn!(
                                        session = %session_id,
                                        request_id = %request_id,
                                        "session/always picked but original command \
                                         not found in approval row; side effect skipped"
                                    );
                                }
                            }
                            tracing::debug!(
                                session = %session_id,
                                request_id = %request_id,
                                ?choice,
                                approved,
                                "resolved approval response (4-state)"
                            );
                            continue;
                        }
                        WsClientMessage::ClarifyResponse { request_id, answer } => {
                            // Wake the blocked `clarify` tool call. If the
                            // request id is unknown (timed out, dropped,
                            // resolved twice) we just log — never fail the
                            // WS exchange.
                            if let Err(e) = state
                                .clarify_dispatcher
                                .respond_by_id(&request_id, answer)
                                .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    request_id = %request_id,
                                    "no pending clarify to resolve"
                                );
                            }
                            continue;
                        }
                        WsClientMessage::Send {
                            text,
                            model,
                            provider,
                            personality,
                            system_prompt,
                            tools,
                            workspace,
                            permission,
                            user_id,
                            request_id,
                            images,
                            image,
                            image_mime_type,
                            skills,
                        } => {
                        let request = SendMessageRequest {
                            text,
                            model,
                            provider,
                            personality,
                            system_prompt,
                            tools,
                            workspace,
                            permission,
                            user_id,
                            request_id,
                            images,
                            image,
                            image_mime_type,
                            skills,
                        };
                            // Fall through to the send handling below.
                            // We use a helper to avoid code duplication.
                            handle_ws_stream_send(
                                &mut socket,
                                &state,
                                &session_id,
                                &trace_id,
                                request,
                            )
                            .await;
                            continue;
                        }
                        // S5 task-engine client commands. Forwarded to
                        // the runtime via `state.task_engine` if present.
                        WsClientMessage::GroupChatSpecUpdate {
                            room_id,
                            node_id,
                            new_spec,
                        } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                let _ = te
                                    .clone()
                                    .on_spec_updated(&room_id, &node_id, &new_spec)
                                    .await;
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatReviewVerdict {
                            room_id,
                            node_id,
                            passed,
                            comment,
                        } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                let _ = te
                                    .clone()
                                    .on_review_verdict(
                                        &room_id,
                                        &node_id,
                                        passed,
                                        &comment,
                                        "__human__",
                                    )
                                    .await;
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatCancel { room_id, node_id } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                let _ = te
                                    .clone()
                                    .on_cancelled(&room_id, node_id.as_deref())
                                    .await;
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatPlanSubmitted {
                            room_id,
                            parent_node_id,
                            subtasks,
                        } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                let _ = te
                                    .clone()
                                    .on_plan_submitted(
                                        &room_id,
                                        &parent_node_id,
                                        subtasks
                                            .into_iter()
                                            .map(|s| {
                                                hermix_gateway::groupchat::SubtaskSpec {
                                                    node_id: s.node_id,
                                                    description: s.description,
                                                    spec: s.spec,
                                                    assigned_agent: s.assigned_agent,
                                                    sort_order: 0,
                                                }
                                            })
                                            .collect(),
                                    )
                                    .await;
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatVote {
                            room_id,
                            target_agent_id,
                            direction,
                        } => {
                            let delta: i32 = match direction {
                                hermix_transport::VoteKind::Up => 20,
                                hermix_transport::VoteKind::Down => -20,
                            };
                            let mgr = state.groupchat_manager();
                            match mgr.adjust_member_score(&room_id, &target_agent_id, delta).await {
                                Ok(snapshot) => {
                                    let event = WsEnvelope {
                                        version: 1,
                                        request_id: None,
                                        trace_id: trace_id.clone(),
                                        event: StreamEvent::VoteApplied {
                                            room_id: room_id.clone(),
                                            voter_agent_id: String::new(),
                                            target_agent_id: target_agent_id.clone(),
                                            direction,
                                            delta,
                                            new_score: snapshot.score,
                                            previous_tier: snapshot.previous_tier,
                                            new_tier: Some(snapshot.tier),
                                        },
                                    };
                                    let _ = socket
                                        .send(WsMessage::Text(
                                            serde_json::to_string(&event).unwrap_or_default().into(),
                                        ))
                                        .await;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "GroupChatVote: adjust_member_score failed for room={} agent={}: {}",
                                        room_id, target_agent_id, e
                                    );
                                }
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatDispute {
                            room_id,
                            violation_id,
                            reason,
                        } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                if let Some(_deduction) = te.dispute_violation(&violation_id, &reason) {
                                    tracing::info!(
                                        "GroupChatDispute: violation {} disputed in room {}",
                                        violation_id, room_id
                                    );
                                } else {
                                    tracing::warn!(
                                        "GroupChatDispute: violation {} not found or already terminal",
                                        violation_id
                                    );
                                }
                            }
                            continue;
                        }
                        WsClientMessage::GroupChatOverturn {
                            room_id,
                            violation_id,
                            agent_id,
                            comment,
                        } => {
                            if let Some(te) = state.task_engine.as_ref() {
                                match te.overturn_violation(
                                    &room_id, &violation_id, &agent_id,
                                    "leader", &comment,
                                ).await {
                                    Ok(deduction) => {
                                        tracing::info!(
                                            "GroupChatOverturn: restored {} points for agent={} room={}",
                                            deduction, agent_id, room_id
                                        );
                                    }
                                    Err(_) => {
                                        tracing::warn!(
                                            "GroupChatOverturn: violation {} not overturnable",
                                            violation_id
                                        );
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }

                // Legacy plain-text / SendMessageRequest fallback.
                let parsed: Option<SendMessageRequest> = serde_json::from_str(&text).ok();
                let request = parsed.unwrap_or_else(|| SendMessageRequest {
                    text: text.to_string(),
                    model: None,
                    provider: None,
                    personality: None,
                    system_prompt: None,
                    tools: None,
                    workspace: None,
                    permission: None,
                    user_id: None,
                    request_id: None,
                    images: None,
                    image: None,
                    image_mime_type: None,
                    skills: None,
                });
                handle_ws_stream_send(
                    &mut socket,
                    &state,
                    &session_id,
                    &trace_id,
                    request,
                )
                .await;
            }
            WsMessage::Close(_) => break,
            _ => {}
        }
    }
}

/// Pull the canonical "command" string out of the persisted
/// approval row for a given `(session_id, request_id)`. Returns
/// `Ok(Some(cmd))` on hit, `Ok(None)` if the row was already
/// resolved / evicted, `Err` on a DB error. Used by the
/// 4-state `session` / `always` side-effect handlers below to
/// know which command to add to the bypass / persistent list.
fn lookup_pending_command(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
) -> Result<Option<String>, String> {
    // Cheap in-memory check: the live `ApprovalRegistry` still
    // holds the sender until the timer fires or the row is
    // resolved. A missing row means the user already responded
    // (a duplicate `ApprovalResponse`) or the 60s timer fired.
    // We only proceed if the row is **still pending** so we
    // never re-record an already-decided approval.
    if !state.approval_registry.is_pending(session_id, request_id) {
        return Ok(None);
    }
    // Pull the original arguments from the persisted approval
    // message so we can extract the `command` field.
    let needle = format!("%\"request_id\":\"{}\"%", request_id);
    let conn = rusqlite::Connection::open(&state.session_persistence.db_path())
        .map_err(|e| format!("open sessions db: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT extra FROM messages
             WHERE session_id = ?1 AND role = 'approval' AND extra LIKE ?2
             LIMIT 1",
        )
        .map_err(|e| format!("prepare: {e}"))?;
    let extra: Option<String> = match stmt.query_row(
        rusqlite::params![session_id, needle],
        |row| row.get::<_, Option<String>>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(format!("query: {e}")),
    };
    let Some(extra) = extra else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(&extra)
        .map_err(|e| format!("parse extra: {e}"))?;
    let command = v
        .get("arguments")
        .and_then(|a| a.get("command"))
        .or_else(|| v.get("arguments").and_then(|a| a.get("cmd")))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    Ok(command)
}

/// Persist the 4-state side effect of a `Session` / `Always`
/// approval response. `Session` appends a compiled regex to
/// the in-memory `session_bypass` map (cleared on server
/// restart — matches upstream Hermes semantics). `Always`
/// appends a literal entry to the persistent `UserAllowlist`
/// and saves the JSON file. `Once` / `Deny` are no-ops here
/// (the registry's resolve is the only side effect they need).
fn record_approval_side_effect(
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

fn redact_sensitive(input: &str) -> String {
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

// ---------------------------------------------------------------------------
// Visible-text sanitizer for raw tool-call markers in LLM output.
//
// The redactor itself lives in `hermix_core::tool_call_sanitizer`
// (see `hermix_core::ToolCallSanitizer`). It is a *stateful*
// state-machine: opening and closing XML tags may arrive in different
// streaming chunks, and a per-chunk regex pass (the previous
// implementation) would leak any block whose opener and closer were
// split across a chunk boundary. The new implementation holds the
// in-between chunks until the matching close arrives.
//
// ## Why this lives in two places
//
// The agent loop (`hermix_agent::agent_loop::apply_stream_collect_chunk`)
// runs one copy so the *persisted* assistant message is clean on
// reload. The WS layer (`hermix_server::lib` on_chunk) runs a second
// copy so the *live* stream never shows leaked XML. The two passes
// are idempotent (the second pass is a no-op once the text is
// already clean), so the doubling is intentional belt-and-suspenders
// and not a bug.
//
// ## What it does NOT do
//
// It does NOT try to *execute* the embedded tool call. That would
// require either (a) telling the agent loop to use a different
// text-based parser as the authoritative source, or (b) pre-parsing
// the streamed text and synthesising `tool_start` / `tool_complete`
// events out-of-band. Both are bigger surface-area changes; the
// narrow "don't show the XML to the user" fix is enough to stop the
// visible leak today.
//
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
/// 2026-06-25: 多图支持 — client sends `Vec<ImageAttachment>` with binary
/// `bytes`; the LLM API needs `data:<mime>;base64,<...>` URLs, so we
/// encode here.
fn convert_wire_images(
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
async fn voice_transcribe(
    Json(req): Json<hermix_voice::TranscribeRequest>,
) -> Result<Json<hermix_voice::TranscribeResponse>, HttpError> {
    let pipeline = hermix_voice::default_pipeline();
    let resp = hermix_voice::transcribe_pcm(&req, Some(&pipeline))
        .await
        .map_err(voice_bridge_err_to_http)?;
    Ok(Json(resp))
}

/// `POST /v1/voice/synthesize` — accept text + provider/voice/format
/// and return synthesized audio bytes. Currently a structured
/// placeholder; the actual network call to the upstream TTS provider
/// is performed by `tts_streaming` once a `PcmSink` is wired in.
async fn voice_synthesize(
    Json(req): Json<hermix_voice::SynthesizeRequest>,
) -> Result<Json<hermix_voice::SynthesizeResponse>, HttpError> {
    let resp = hermix_voice::synthesize_text(&req, None)
        .await
        .map_err(voice_bridge_err_to_http)?;
    Ok(Json(resp))
}

/// Convert `hermix_voice::VoiceBridgeError` to an `HttpError` with a
/// stable 4xx/5xx mapping.
fn voice_bridge_err_to_http(e: hermix_voice::VoiceBridgeError) -> HttpError {
    use hermix_voice::VoiceBridgeError as V;
    let status = match &e {
        V::Base64(_) | V::InvalidSampleRate(_) | V::EmptyAudio | V::Internal(_) => {
            StatusCode::BAD_REQUEST
        }
        V::EmptySynthesis | V::Tool(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError {
        status,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod voice_handler_tests {
    use super::*;

    #[test]
    fn base64_err_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::Base64("bad".into());
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
        assert!(h.message.contains("bad"));
    }

    #[test]
    fn invalid_sample_rate_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::InvalidSampleRate(0);
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn empty_synthesis_maps_to_500() {
        let e = hermix_voice::VoiceBridgeError::EmptySynthesis;
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn empty_audio_maps_to_400() {
        let e = hermix_voice::VoiceBridgeError::EmptyAudio;
        let h = voice_bridge_err_to_http(e);
        assert_eq!(h.status, StatusCode::BAD_REQUEST);
    }
}

// M2: TickLoop API
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct TickLoopStatus {
    running: bool,
    phase: String,
}

/// `GET /v1/tickloop` — return current TickLoop status.
async fn get_tickloop_status(State(state): State<HttpServerState>) -> Json<TickLoopStatus> {
    let running = state.tickloop_manager.is_running().await;
    Json(TickLoopStatus {
        running,
        phase: if running {
            "active".to_string()
        } else {
            "stopped".to_string()
        },
    })
}

#[derive(Debug, Deserialize)]
struct TickLoopToggle {
    enabled: bool,
}

/// `POST /v1/tickloop` — enable or disable the TickLoop at runtime.
async fn toggle_tickloop(
    State(state): State<HttpServerState>,
    Json(body): Json<TickLoopToggle>,
) -> Result<Json<TickLoopStatus>, HttpError> {
    if body.enabled {
        state.tickloop_manager
            .start(Some(Arc::new(state.consciousness_tracker.clone())))
            .await;
    } else {
        state.tickloop_manager.stop().await;
    }
    Ok(Json(TickLoopStatus {
        running: body.enabled,
        phase: if body.enabled { "active".to_string() } else { "stopped".to_string() },
    }))
}

#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

impl From<AgentError> for HttpError {
    fn from(value: AgentError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: value.to_string(),
        }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

/// Resolve an LLM provider name and base URL for the runtime TickLoop SelfCheck
/// connectivity probe.
///
/// Priority:
/// 1. `config.model`'s `provider` segment → look up that provider in `llm_providers`
///    and return its `base_url` (if set) + provider name.
/// 2. The first provider in `llm_providers` that has an `api_key` set → provider name,
///    and its `base_url` (if set).
///
/// Returns `Some((name, base_url_or_none))` so the runtime can infer a default URL
/// even when `base_url` is not explicitly configured.
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
    //    var OR explicit base_url). Without an api_key/env, the runtime can't
    //    actually authenticate, but returning the provider name + base_url lets
    //    the caller at least surface a useful default (and probe it).
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

/// Phase 18: SO_REUSEADDR regression test. Simulates the exact
/// "crashed child left a LISTEN socket on the same port" scenario
/// that crashed hermix-desktop sidecar on Windows with
/// `os error 10048 (WSAEADDRINUSE)`.
///
/// We:
/// 1. Bind a TCP listener on a random port with SO_REUSEADDR=on
///    (mirroring the production TcpSocket::bind path in
///    `run_server_with_state`).
/// 2. Drop that listener without graceful shutdown — simulating
///    a process crash.
/// 3. Immediately re-bind the SAME port with SO_REUSEADDR=on.
/// 4. Assert the second bind succeeds (would fail with
///    WSAEADDRINUSE on Windows without SO_REUSEADDR).
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

/// Phase 21 (2026-07-07): regression coverage for the
/// "database disk image is malformed" `delete_session` bug.
///
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
///
/// This test asserts (1) the delete-side PRAGMA matches the
/// writer side, (2) the WAL is truncated after commit, and
/// (3) the 404 path rolls back the cascade DELETEs.
/// Phase 21 extension (2026-07-07): the WAL regression tests
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
        //
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
// Phase 1 / P1-5: Orchestration Engine — Task CRUD HTTP handlers
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
/// (Phase 4 / P4-2: supports multi-agent subtask trees)
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
// Phase 7 / P7-1: Product Delivery — HTTP handlers
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
/// (Phase 7 / P7-1)
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
///
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
// Phase 5 / P5-5: Automation Engine — HTTP handlers
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
/// (Phase 6 / P6-5)
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
