//! WS-layer coalescer for parallel tool-call approvals.
//!
//! When the LLM emits N high-risk tool calls in the same synchronous
//! pre-dispatch pass, the agent loop fires N `on_event("request_approval", …)`
//! hooks back-to-back (microsecond-scale intervals). Pushing one
//! `WsEnvelope` per item would render N independent cards in the
//! desktop UI — the user wants a single "approve all" click instead.
//!
//! [`ApprovalCoalescer`] buffers these per-session, then on a short
//! debounce window (default 50ms) flushes them as **one**
//! [`StreamEvent::RequestApprovalsBatch`] envelope carrying all
//! request_ids under one batch_id. The 50ms window is generous given
//! that the upstream emissions are synchronous; it is not a polling
//! cadence, just enough to absorb any minor scheduling jitter so two
//! pushes from the same `pre_dispatch` pass never straddle two
//! windows in practice. If a push lands *after* the timer disarms, a
//! fresh timer is armed — that case represents a *new* batch (the
//! old one already flushed with its own batch_id).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermix_transport::{RequestApprovalItem, StreamEvent, WsEnvelope};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::approval::ApprovalRegistry;

/// Default coalescing window (milliseconds). Sized so a single
/// `pre_dispatch` pass with N high-risk calls reliably coalesces, but
/// not so long that the user perceives a delay before the batch
/// card appears.
pub const DEFAULT_COALESCE_WINDOW_MS: u64 = 50;

/// Thin alias so callers don't need to reach for the full
/// `tokio::sync::mpsc::Sender` type. The channel is the same one
/// already used by `on_event` to push `WsEnvelope`s.
pub type Tx = mpsc::Sender<WsEnvelope>;

/// Per-session coalescer state. Cloning a coalescer clones the inner
/// `Arc`, so all clones share the same per-session buffers.
#[derive(Clone)]
pub struct ApprovalCoalescer {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Tx,
    registry: ApprovalRegistry,
    pending: Mutex<HashMap<String, Vec<RequestApprovalItem>>>,
    /// Per-session "a flush task is armed" marker. Maps session_id to
    /// `()` — the value is irrelevant; presence alone means the
    /// existing timer hasn't fired yet and a fresh push should
    /// **not** start a second timer.
    armed: Mutex<HashMap<String, ()>>,
    window: Duration,
    /// Identifier used as the envelope's `request_id` field for
    /// downstream tracing. Distinct per WS session in practice.
    envelope_label: String,
    trace_id: String,
}

impl ApprovalCoalescer {
    /// Build a coalescer wired to the same channel the rest of the
    /// `on_event` path uses (so individual `RequestApproval` envelopes
    /// for non-coalesced sessions and batch envelopes for the rest
    /// share the same backpressure surface).
    pub fn new(
        tx: Tx,
        registry: ApprovalRegistry,
        window_ms: u64,
        envelope_label: String,
        trace_id: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                tx,
                registry,
                pending: Mutex::new(HashMap::new()),
                armed: Mutex::new(HashMap::new()),
                window: Duration::from_millis(window_ms),
                envelope_label,
                trace_id,
            }),
        }
    }

    /// Convenience constructor using [`DEFAULT_COALESCE_WINDOW_MS`].
    pub fn with_default_window(
        tx: Tx,
        registry: ApprovalRegistry,
        envelope_label: String,
        trace_id: String,
    ) -> Self {
        Self::new(
            tx,
            registry,
            DEFAULT_COALESCE_WINDOW_MS,
            envelope_label,
            trace_id,
        )
    }

    /// Append a pending approval item to this session's batch. If this
    /// is the first item in the window for this session, arms a new
    /// flush timer; otherwise the existing timer will drain the
    /// newly-appended item along with whatever was already queued.
    pub fn push(&self, session_id: String, item: RequestApprovalItem) {
        let newly_armed = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.entry(session_id.clone()).or_default().push(item);
            let mut armed = self.inner.armed.lock().unwrap_or_else(|e| e.into_inner());
            armed.insert(session_id.clone(), ()).is_none()
        };
        if newly_armed {
            let inner = Arc::clone(&self.inner);
            let sid = session_id;
            tokio::spawn(async move {
                tokio::time::sleep(inner.window).await;
                inner.flush(sid).await;
            });
        }
    }
}

impl Inner {
    /// Drain the session's pending items and emit one
    /// `RequestApprovalsBatch` envelope. Called from a spawned task
    /// after the window elapses.
    async fn flush(&self, session_id: String) {
        let items = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove(&session_id).unwrap_or_default()
        };
        // Disarm *before* the await so a late push (which would have
        // missed this window) starts a fresh timer. Scoping the
        // guard to an inner block ensures NLL sees it released by
        // the time we hit `tx.send(...).await` below — `std::sync::MutexGuard`
        // is `!Send`, so holding it across an await would break
        // `tokio::spawn`'s `Future: Send` bound.
        {
            let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
            armed.remove(&session_id);
        }

        if items.is_empty() {
            return;
        }
        let batch_id = Uuid::new_v4().to_string();
        let request_ids = items.iter().map(|i| i.request_id.clone()).collect();
        // Record the batch → request_ids mapping up front so a fast
        // response from the client still finds the batch when
        // `resolve_batch` is called.
        self.registry
            .register_batch(&session_id, &batch_id, request_ids);
        let envelope = WsEnvelope {
            version: 1,
            request_id: Some(batch_id.clone()),
            trace_id: self.trace_id.clone(),
            event: StreamEvent::RequestApprovalsBatch {
                batch_id,
                items,
            },
        };
        if let Err(e) = self.tx.send(envelope).await {
            // Same channel-closed semantics as the single-item
            // `RequestApproval` path (see `ws.rs::on_event`). Log at
            // debug — a disconnected WS is the only failure mode and
            // is not actionable.
            tracing::debug!(
                error = %e,
                session_id = %session_id,
                "approval batch envelope dropped: WS receiver closed"
            );
        }
        // Suppress unused-warning for `envelope_label` — currently
        // only `request_id` / `trace_id` are propagated to the
        // envelope, but the label is kept here so a future change
        // (e.g. logging which session's coalescer drained) has the
        // identifying string in scope.
        let _ = &self.envelope_label;
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end tests for the 50ms window: a tight loop of `push`
    //! calls lands one envelope carrying all items; a fresh `push`
    //! after the flush arms a new timer (no permanent starvation);
    //! `ApprovalRegistry::resolve_batch` finds every item under the
    //! emitted `batch_id`.

    use super::*;
    use std::time::Duration;

    fn item(rid: &str) -> RequestApprovalItem {
        RequestApprovalItem {
            request_id: rid.to_string(),
            tool: "terminal".to_string(),
            arguments: serde_json::json!({"command": "echo hi"}),
            risk: "caution".to_string(),
            reason: None,
            expires_at: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn coalesces_three_pushes_into_one_envelope() {
        let registry = ApprovalRegistry::new();
        let (tx, mut rx) = mpsc::channel::<WsEnvelope>(8);
        let coalescer = ApprovalCoalescer::new(
            tx,
            registry.clone(),
            10, // 10ms window — fast for tests, still larger than a
            // synchronous `pre_dispatch` pass.
            "label".into(),
            "trace".into(),
        );
        let sid = "sess-A";
        // In production the agent loop synchronously calls
        // `ApprovalRegistry::request_approval` for each tool call
        // *and* the coalescer flushes the WS envelope — both
        // registrations happen before the user clicks. Mirror that
        // here so `resolve_batch` actually finds the receivers.
        let _rxs: Vec<_> = (0..3)
            .map(|i| registry.request_approval(sid, &format!("req-{i}")))
            .collect();
        // Three back-to-back pushes simulate a single LLM turn
        // emitting N parallel high-risk tool calls.
        for i in 0..3 {
            coalescer.push(sid.to_string(), item(&format!("req-{i}")));
        }
        let envelope = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("flush did not arrive within 500ms")
            .expect("channel closed");
        match envelope.event {
            StreamEvent::RequestApprovalsBatch { batch_id, items } => {
                assert_eq!(items.len(), 3, "expected exactly 3 items in the batch");
                let ids: Vec<String> = items.iter().map(|i| i.request_id.clone()).collect();
                assert_eq!(ids, vec!["req-0", "req-1", "req-2"]);
                // Registry must have the batch mapped to all 3 rids
                // so `resolve_batch` can fan out.
                let pairs = registry.resolve_batch(
                    sid,
                    &batch_id,
                    hermix_core::ApprovalOutcome::Approved,
                );
                assert_eq!(pairs.len(), 3);
                assert!(pairs.iter().all(|(_, ok)| *ok));
            }
            other => panic!("expected RequestApprovalsBatch, got {other:?}"),
        }
        // No second envelope leaks from the first batch's window.
        let leak = tokio::time::timeout(Duration::from_millis(120), rx.recv()).await;
        assert!(leak.is_err(), "unexpected second envelope: {leak:?}");
    }

    #[tokio::test]
    async fn push_after_flush_starts_a_fresh_timer() {
        let registry = ApprovalRegistry::new();
        let (tx, mut rx) = mpsc::channel::<WsEnvelope>(8);
        let coalescer = ApprovalCoalescer::new(
            tx,
            registry.clone(),
            10,
            "label".into(),
            "trace".into(),
        );
        coalescer.push("sess-A".to_string(), item("req-a"));
        let env1 = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("first flush did not arrive")
            .expect("channel closed");
        assert!(matches!(
            env1.event,
            StreamEvent::RequestApprovalsBatch { .. }
        ));
        // Second push — must start a fresh timer, not be lost.
        coalescer.push("sess-A".to_string(), item("req-b"));
        let env2 = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("second flush did not arrive")
            .expect("channel closed");
        if let StreamEvent::RequestApprovalsBatch { items, .. } = env2.event {
            assert_eq!(items.len(), 1, "fresh batch must not include req-a");
            assert_eq!(items[0].request_id, "req-b");
        } else {
            panic!("expected RequestApprovalsBatch");
        }
    }

    #[tokio::test]
    async fn empty_session_disarms_without_emit() {
        // Defensive: arming a timer with no items queued (impossible
        // via the public API — `push` always queues at least one)
        // is tested via a manual flush to make sure disarming is
        // idempotent and doesn't emit a stray envelope.
        let registry = ApprovalRegistry::new();
        let (tx, mut rx) = mpsc::channel::<WsEnvelope>(4);
        let coalescer = ApprovalCoalescer::new(
            tx,
            registry,
            10,
            "label".into(),
            "trace".into(),
        );
        // Direct flush on a never-pushed session.
        coalescer.inner.flush("ghost".to_string()).await;
        // Should not emit anything.
        let leak = tokio::time::timeout(Duration::from_millis(80), rx.recv()).await;
        assert!(leak.is_err(), "flush on empty session must not emit");
    }
}