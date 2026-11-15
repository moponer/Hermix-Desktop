//! Soft-disconnect grace logic for the WebSocket streaming layer.
//!
//! When the WS forward loop in `ws.rs` hits a transient failure
//! (rx-send-fail, heartbeat-ping-fail, ws close frame, etc.), it no
//! longer hard-aborts the agent loop. Instead it schedules a
//! **grace timer** that, after `grace_secs`:
//!   - notifies the agent so any in-flight tool approval wait wakes
//!     up as `ChannelClosed` (the agent then writes the denial
//!     note, falls into the next LLM call, and the turn ends
//!     cleanly);
//!   - removes the `active_handles[active_key]` entry.
//!
//! `active_sessions` is NOT released here (C3 会话级互斥): the session
//! slot is keyed by pure `session_id` and must stay held while the agent
//! runs — releasing it would let a reconnecting client spawn a second
//! agent for the same session while the first is still autonomous. A
//! reconnecting send is instead declined via the `task_in_flight`
//! path in `ws/mod.rs`, and the agent's own completion cleanup frees
//! the slot.
//!
//! Two follow-on risks were fixed:
//!
//! 1. The grace timer must be cancellable — otherwise a reconnect's
//!    `takeover` (which aborts the old agent handle and inserts a
//!    fresh one for the same `active_key`) would have its brand-new
//!    handle yanked 90s later. We keep the `JoinHandle` in
//!    `pending_grace` keyed by `active_key`; the timer self-
//!    removes on fire, and the takeover path aborts the handle
//!    before deleting.
//!
//! 2. If the agent finishes but the `Done` frame is lost in the
//!    disconnect race, a reconnect that re-sends the same
//!    `(session_id, text)` would otherwise spawn a fresh agent and
//!    double-execute non-idempotent tools. We record the final
//!    content in `completed_keys`; on entry we hit-test and serve
//!    a synthetic `Done` frame instead of re-running.
//!
//! All helpers take ** field-level Arc references ** (not
//! `&HttpServerState`) so they can be called from inside
//! `tokio::spawn(async move { ... })` closures — which require
//! `'static` lifetimes and cannot hold `&HttpServerState`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Schedule a timer that fires after `grace_secs`:
///   1. Self-remove the entry from `pending_grace` (so a later
///      reconnect takeover that arrives *after* the timer fires
///      doesn't try to abort an already-completed JoinHandle).
///   2. `notify.notify_one()` — wakes any `shutdown_notify.wait()`
///      in the agent loop so it self-drains via the same path used
///      by an explicit user Stop.
///   3. Remove `active_handles[key]` as a final guard against the
///      agent task forgetting to clean up after itself on the
///      approval-wakeup branch.
///
/// The `JoinHandle` is registered in `pending_grace[key]` so a
/// reconnecting client's `abort_pending_grace()` call can cancel
/// the timer before it fires.
pub fn schedule_grace_disconnect(
    active_handles: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pending_grace: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    active_key: &str,
    notify: Arc<Notify>,
    grace_secs: u64,
) {
    let ah = active_handles.clone();
    let pg = pending_grace.clone();
    let gk = active_key.to_string();

    let timer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(grace_secs)).await;
        // Self-remove first so a concurrent `abort_pending_grace`
        // sees the slot empty (no-op) rather than racing to abort
        // an already-fired timer.
        if let Ok(mut g) = pg.lock() {
            g.remove(&gk);
        }
        // Autonomous-continuation policy: a WS disconnect MUST NOT
        // interrupt the agent *loop*. The agent keeps running in the
        // background and persists its result; a reconnecting client
        // resumes via the `completed_keys` replay path (or observes
        // the still-running task).
        //
        // We still fire `notify.notify_one()` so any *pending approval
        // wait* wakes immediately as `ChannelClosed` and the agent
        // self-drains that turn (auto-denies + continues) instead of
        // blocking until the 30s approval timeout. What we no longer
        // do is call `InterruptController::interrupt` to halt future
        // turns — that removal lives in `local_agent_service.rs`.
        //
        // `active_handles` cleanup is gated on the agent having
        // actually finished. If the agent is still alive at grace
        // time we leave the entry in place — removing it would open
        // a double-spawn window if a reconnect arrives while the
        // autonomous task is running. The agent removes its own
        // `active_handles` entry on natural completion (see
        // `handle_ws_stream_send`'s completion block in ws.rs).
        notify.notify_one();
        let finished = ah
            .lock()
            .ok()
            .and_then(|g| g.get(&gk).map(|h| h.is_finished()))
            .unwrap_or(false);
        if finished {
            if let Ok(mut g) = ah.lock() {
                g.remove(&gk);
            }
        }
    });

    if let Ok(mut g) = pending_grace.lock() {
        g.insert(active_key.to_string(), timer);
    }
}

/// Cancel a still-pending grace timer for `active_key`. Called by
/// the takeover path when a reconnect aborts the previous agent
/// handle: the new handle inserted under the same key would
/// otherwise be wiped by the still-queued timer.
///
/// Returns true if a timer was actually cancelled (handy for
/// assertion in tests; production callers ignore the return).
pub fn abort_pending_grace(
    pending_grace: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    active_key: &str,
) -> bool {
    let timer = match pending_grace.lock() {
        Ok(mut g) => g.remove(active_key),
        Err(p) => p.into_inner().remove(active_key),
    };
    if let Some(t) = timer {
        t.abort();
        return true;
    }
    false
}

/// Look up the final answer for a `(session_id, text)` pair that
/// has already completed in a previous turn. Returns `Some(content)`
/// if a synthetic `Done` frame should be served without spawning a
/// fresh agent.
///
/// `take()` semantics: the entry is NOT removed — it must remain
/// available for any further reconnects that arrive within the
/// brief window where the client might double-resend. A deliberate
/// re-run for the same prompt clears it via [`invalidate_completed`].
pub fn take_completed_replay(
    completed_keys: &Arc<Mutex<HashMap<String, String>>>,
    active_key: &str,
) -> Option<String> {
    match completed_keys.lock() {
        Ok(g) => g.get(active_key).cloned(),
        Err(p) => p.into_inner().get(active_key).cloned(),
    }
}

/// Clear a completed-key entry. Called at the *start* of a fresh
/// agent spawn so a deliberate re-run for the same prompt
/// supersedes the replay shortcut.
pub fn invalidate_completed(
    completed_keys: &Arc<Mutex<HashMap<String, String>>>,
    active_key: &str,
) {
    if let Ok(mut g) = completed_keys.lock() {
        g.remove(active_key);
    }
}

/// Record the final answer so a future reconnect that lost the
/// `Done` frame can short-circuit via [`take_completed_replay`].
pub fn record_completed(
    completed_keys: &Arc<Mutex<HashMap<String, String>>>,
    active_key: &str,
    content: String,
) {
    if let Ok(mut g) = completed_keys.lock() {
        g.insert(active_key.to_string(), content);
    }
}

// ============================================================================
// Unit tests — drive the pub helpers directly. No axum router, no WS, no
// agent loop required. Tests use `grace_secs = 0` or very small values so
// the suite completes in ms, not 90 s.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state;
    use tokio::sync::Notify;

    /// Build an `HttpServerState` with empty registries. We rely
    /// on `test_state::empty_state` to avoid depending on the full
    /// `build()` path (which needs gateway config + agent service).
    fn fresh_state() -> crate::state::HttpServerState {
        test_state::empty_state()
    }

    // ---------------------- schedule_grace_disconnect ----------------------

    #[tokio::test]
    async fn grace_does_not_release_active_sessions_slot() {
        // C3 会话级互斥：session 槽由 agent 任务自己的收尾清理释放；
        // grace 断连不得提前放行，否则重连会在旧 agent 仍在跑时再
        // spawn 出第二个任务。
        let state = fresh_state();
        let key = "s1::hello";
        state
            .active_sessions
            .lock()
            .unwrap()
            .insert("s1".to_string());

        let notify = Arc::new(Notify::new());
        schedule_grace_disconnect(
            &state.active_handles,
            &state.pending_grace,
            key,
            notify,
            0,
        );

        assert!(
            state.active_sessions.lock().unwrap().contains("s1"),
            "grace must NOT release the session slot while the agent may still run"
        );
    }

    #[tokio::test]
    async fn grace_registers_pending_timer() {
        let state = fresh_state();
        let key = "s1::hello";
        let notify = Arc::new(Notify::new());

        schedule_grace_disconnect(
            &state.active_handles,
            &state.pending_grace,
            key,
            notify,
            60,
        );

        assert!(
            state.pending_grace.lock().unwrap().contains_key(key),
            "pending_grace[{key}] must hold the timer handle"
        );
    }

    #[tokio::test]
    async fn grace_timer_fires_after_grace_secs_and_cleans_active_handles() {
        let state = fresh_state();
        let key = "s1::hello";
        let notify = Arc::new(Notify::new());

        // Seed an active_handles entry — the timer should clear it.
        let sentinel = tokio::spawn(async {});
        state
            .active_handles
            .lock()
            .unwrap()
            .insert(key.to_string(), sentinel);

        // 1-second grace keeps the test fast (~1s wall clock).
        schedule_grace_disconnect(
            &state.active_handles,
            &state.pending_grace,
            key,
            notify.clone(),
            1,
        );

        // Right after schedule: pending_grace has the handle,
        // active_handles still has the sentinel.
        assert!(state.pending_grace.lock().unwrap().contains_key(key));
        assert!(state.active_handles.lock().unwrap().contains_key(key));

        // Wait past grace_secs for the timer to fire.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // Timer self-removed from pending_grace.
        assert!(
            !state.pending_grace.lock().unwrap().contains_key(key),
            "pending_grace must be self-cleaned after timer fires"
        );
        // active_handles cleaned.
        assert!(
            !state.active_handles.lock().unwrap().contains_key(key),
            "active_handles must be cleared by the timer"
        );
    }

    #[tokio::test]
    async fn abort_pending_grace_cancels_timer_before_fire() {
        let state = fresh_state();
        let key = "s1::hello";
        let notify = Arc::new(Notify::new());

        let sentinel = tokio::spawn(async {});
        state
            .active_handles
            .lock()
            .unwrap()
            .insert(key.to_string(), sentinel);

        schedule_grace_disconnect(
            &state.active_handles,
            &state.pending_grace,
            key,
            notify,
            5,
        );

        // Takeover path: cancel BEFORE grace_secs elapses.
        let cancelled = abort_pending_grace(&state.pending_grace, key);
        assert!(cancelled, "abort must report a timer was cancelled");

        // Wait well past grace_secs — timer should NOT have fired
        // (cancelled), so active_handles[key] survives.
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        assert!(
            state.active_handles.lock().unwrap().contains_key(key),
            "active_handles must NOT be cleared by a cancelled timer"
        );
        assert!(
            !state.pending_grace.lock().unwrap().contains_key(key),
            "pending_grace must be drained by abort"
        );
    }

    #[test]
    fn abort_pending_grace_returns_false_when_no_timer() {
        let state = fresh_state();
        assert!(!abort_pending_grace(&state.pending_grace, "no-such-key"));
    }

    // ---------------------- completed_keys replay ----------------------------

    #[test]
    fn record_then_take_returns_content() {
        let state = fresh_state();
        let key = "s1::hello";
        record_completed(&state.completed_keys, key, "the answer is 42".to_string());

        let replay = take_completed_replay(&state.completed_keys, key);
        assert_eq!(replay.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn take_is_non_consuming() {
        // Repeated replays for the same key must all return the
        // stored content — the entry stays until invalidate_completed
        // is called on a fresh spawn.
        let state = fresh_state();
        let key = "s1::hello";
        record_completed(&state.completed_keys, key, "answer".to_string());

        assert_eq!(
            take_completed_replay(&state.completed_keys, key).as_deref(),
            Some("answer")
        );
        assert_eq!(
            take_completed_replay(&state.completed_keys, key).as_deref(),
            Some("answer")
        );
        assert_eq!(
            take_completed_replay(&state.completed_keys, key).as_deref(),
            Some("answer")
        );
    }

    #[test]
    fn take_returns_none_when_missing() {
        let state = fresh_state();
        assert_eq!(
            take_completed_replay(&state.completed_keys, "never-ran"),
            None
        );
    }

    #[test]
    fn invalidate_clears_entry() {
        let state = fresh_state();
        let key = "s1::hello";
        record_completed(&state.completed_keys, key, "answer".to_string());
        invalidate_completed(&state.completed_keys, key);

        assert_eq!(take_completed_replay(&state.completed_keys, key), None);
    }

    #[test]
    fn invalidate_unknown_key_is_noop() {
        // Must not panic on missing key.
        let state = fresh_state();
        invalidate_completed(&state.completed_keys, "never-existed");
    }

    // ---------------------- interaction ------------------------------------

    #[tokio::test]
    async fn takeover_then_record_does_not_orphan_timer() {
        // Scenario: WS drops (schedule grace) → reconnect arrives
        // within grace (abort) → fresh agent runs → completes →
        // record_completed is called. Verify no panic, no leftover
        // timer.
        let state = fresh_state();
        let key = "s1::hello";
        let notify = Arc::new(Notify::new());

        schedule_grace_disconnect(
            &state.active_handles,
            &state.pending_grace,
            key,
            notify,
            60,
        );
        let _ = abort_pending_grace(&state.pending_grace, key);
        record_completed(&state.completed_keys, key, "fresh answer".to_string());

        assert_eq!(
            take_completed_replay(&state.completed_keys, key).as_deref(),
            Some("fresh answer")
        );
        assert!(state.pending_grace.lock().unwrap().is_empty());
    }
}