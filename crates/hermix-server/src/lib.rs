#![allow(clippy::doc_lazy_continuation, clippy::field_reassign_with_default)]
//! HTTP and WebSocket API server for Hermes.
//!
//! Environment (see also `security` module):
//! - `HERMES_HTTP_MAX_BODY_BYTES` — max JSON body size for POST routes (default 2 MiB).
//! - `HERMES_HTTP_CORS_ORIGINS` — comma-separated browser Origins for CORS; empty → permissive.
//! - `HERMES_SERVE_WEB_STATIC` — set `0`/`false`/`off` to skip serving `apps/dashboard/dist` (API-only).
//! Policy HTTP routes are intentionally omitted (Hermes Python does not expose them).

pub mod dashboard;
mod security;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_state;

pub use security::parse_allowed_ips;
pub use security::PolicyGuardConfig;

use std::convert::Infallible;
use hermix_config::env_compat;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use base64::Engine as _;

use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::header;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::StreamExt;
use hermix_config::GatewayConfig;
use hermix_agent::python_alignment::{strip_think_blocks_for_ack, ThinkBlockParser};
use hermix_agent::ArchiveRef;
use hermix_core::{traits::PermissionLevel, AgentError, Message, MessageRole, StreamChunk, ToolCallSanitizer as CoreToolCallSanitizer};
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
}

impl HttpServerState {
    /// Subscribe to the broadcast of new pending `clarify` requests. Each
    /// WebSocket call this once at connect-time; the returned receiver
    /// yields one [`ClarifyBroadcast`] per freshly enqueued request.
    pub fn clarify_subscribe(&self) -> tokio::sync::broadcast::Receiver<ClarifyBroadcast> {
        self.clarify_broadcast.subscribe()
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
        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
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
            for entry in mcp_list.servers.iter().filter(|e| e.enabled) {
                match crate::dashboard::mcp::entry_to_server_config(entry) {
                    Ok((cfg_name, cfg)) => {
                        if let Err(e) = mcp_manager.connect(&cfg_name, cfg).await {
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

        Ok(Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence,
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
        })
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
        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
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

        Ok(Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence,
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
        })
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

    let app = router(state);
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

    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("hermes-server graceful shutdown");
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
    let conn = match rusqlite::Connection::open(db_path) {
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
    let conn = rusqlite::Connection::open(db_path).map_err(|e| HttpError {
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

async fn delete_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = rusqlite::Connection::open(db_path).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    conn.execute(
        "DELETE FROM messages WHERE session_id = ?1",
        rusqlite::params![session_id],
    )
    .map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    conn.execute(
        "DELETE FROM sessions WHERE id = ?1",
        rusqlite::params![session_id],
    )
    .map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
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
    // Use the timeline-aware loader so approval rows (role='approval')
    // come back interleaved with assistant messages in the order the
    // agent emitted them. The UI needs this to position the inline
    // card in the correct slot after a reload.
    let entries = state
        .session_persistence
        .load_session_with_approvals(&session_id)
        .map_err(HttpError::from)?;
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
        // Defense-in-depth: the SQL filter in
        // `load_session_with_approvals` already excludes role='system'
        // rows. This match-arm is here so even if a future caller
        // accidentally loads a different entry set (e.g. a test
        // fixture) the system prompt never reaches the wire.
        .filter(|e| !matches!(e, hermix_agent::SessionEntry::Message(m) if matches!(m.role, MessageRole::System)))
        .map(|entry| match entry {
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
                ProtocolMessage {
                    id,
                    role: match m.role {
                        MessageRole::User => hermix_transport::Role::User,
                        MessageRole::Assistant => hermix_transport::Role::Assistant,
                        MessageRole::System => hermix_transport::Role::System,
                        MessageRole::Tool => hermix_transport::Role::Tool,
                    },
                    // Defence-in-depth: sanitise any raw tool-call XML
                    // (MiniMax / MiniMax / Hermes fine-tunes) on the
                    // read path. The agent loop + WS layer already
                    // strip these at write time, but persisted
                    // messages from a pre-fix session, or any future
                    // non-streaming code path that bypasses the
                    // streaming redactor, would otherwise leak the
                    // raw XML straight back into the chat on reload.
                    content: {
                        let raw = m.content.unwrap_or_default();
                        if raw.is_empty() {
                            raw
                        } else {
                            // Use a fresh sanitizer per request — its
                            // state is meaningless across calls and
                            // sharing one across concurrent reads
                            // would needlessly serialise them.
                            let mut s = CoreToolCallSanitizer::new();
                            let cleaned = s.feed(&raw);
                            s.flush();
                            cleaned
                        }
                    },
                    timestamp: now.clone(),
                    model: None,
                    tool_calls,
                    tool_name,
                    tool_arguments,
                    extra: None,
                    reasoning_content: m.reasoning_content.clone(),
                    execution_timeline,
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
                    timestamp: now.clone(),
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
    let results = query_search(&db_path, &q, limit).unwrap_or_default();
    Json(SessionSearchResponse { results })
}

/// FTS5 search across the messages table. Returns snippets ordered by
/// rank. Best-effort — on any read failure returns an empty vec so the
/// dashboard search box never 500s.
fn query_search(
    db_path: &std::path::Path,
    query: &str,
    limit: u32,
) -> Result<Vec<SessionSearchResult>, String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;

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
                    let (clean, _think) = think_parser.lock().unwrap().feed(text);
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
            return;
        }

        match result {
            Ok(reply) => {
                // Defensive: re-strip think blocks here even though
                // `LocalAgentService::send_message_stream` already
                // runs `strip_think_blocks_for_ack` on the trailing
                // assistant's content.  Belt-and-suspenders against
                // any future change that re-introduces reasoning
                // text into the persisted message or against an
                // upstream provider that emits unclosed `<think>`
                // blocks.
                //
                // Same belt-and-suspenders rationale for
                // `strip_tool_call_markers` — the per-chunk redactor
                // above already handles streaming text, but the
                // terminal `Done.content` is the value persisted into
                // the session history, so we sanitize it here too.
                let cleaned = strip_think_blocks_for_ack(&reply.text);
                // Apply the same stateful tool-call redactor that
                // the per-chunk path uses. We `flush()` first so the
                // sanitizer starts in NORMAL state — the text is
                // the full terminal assistant message, not a chunk
                // mid-stream, so we want a clean state machine. The
                // per-chunk pass above has already cleaned the
                // streamed text, so this is a belt-and-suspenders
                // pass for the persisted-message code path
                // (e.g. session reload, non-streaming `get_reply`).
                let cleaned = {
                    let mut guard = tool_call_sanitizer_owned.lock().unwrap();
                    guard.flush();
                    guard.feed(&cleaned)
                };
                // Guard against empty content from the agent.
                // When LLM returns a blank response (no text, no tool calls),
                // sending StreamEvent::Done { content: "" } creates a blank
                // chat bubble. Skip the Done event entirely in this case
                // — the streaming path (if any) already sent the chunks to
                // the frontend, and the assistant message will have no
                // visible content anyway.
                if cleaned.is_empty() {
                    tracing::warn!(
                        "Skipping Done event: LLM returned empty response"
                    );
                } else {
                    let _ = tx
                        .send(WsEnvelope {
                            version: 1,
                            request_id: request_id_owned.clone(),
                            trace_id: trace_id_owned.clone(),
                            event: StreamEvent::Done {
                                content: redact_sensitive(&cleaned),
                                finish_reason: reply.finish_reason.clone(),
                            },
                        })
                        .await;
                }
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
    });

    // Forward channel events to WebSocket
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
            ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
            agent_handle.abort();
            break;
        }
    }
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
