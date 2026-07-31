//! In-memory per-run trace recorder (Telemetry future② — Tracing, ).
//!
//! Goal: give the agent loop / groupchat runtime a queryable per-`trace_id`
//! timeline of phase boundaries, tool calls, and LLM call start/end — useful
//! for debugging multi-agent runs and matching the eventual openai-agents-
//! python `Tracing` parity. This is deliberately an in-memory bounded buffer
//! (no SQLite yet) so the API surface stays small and easy to evolve: future
//! stages can swap the storage backend without changing call sites.
//!
//! Usage:
//! ```ignore
//! let recorder = RunTraceRecorder::new(64);
//! recorder.start("trace-1");
//! recorder.record("trace-1", TraceEventKind::PhaseStart {
//!     phase: "LlmCall".into(), turn: 0,
//! });
//! // ...
//! let trace = recorder.snapshot("trace-1").unwrap();
//! ```
//!
//! The recorder is bounded by `max_traces`; inserting a new run when full
//! evicts the oldest. `RunTraceRecorder` is `Send + Sync` and is safe to share
//! behind an `Arc` in the agent loop / WS server.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Snapshot of all events recorded for a single `trace_id` run.
///
/// Cheap to clone — used both by the in-memory store and any future
/// persistence/export path (Prometheus exposition, OTLP span bridge, SQLite
/// persistence, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunTrace {
    pub trace_id: String,
    /// Unix-epoch millis when `start()` was first called for this trace_id.
    /// Subsequent `record()` events carry `ts_ms` relative to this anchor.
    pub started_at_ms: u64,
    /// Ordered list of events in record-time order.
    pub events: Vec<TraceEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceEvent {
    /// Milliseconds elapsed since the `RunTrace.started_at_ms` anchor.
    /// Using a relative offset keeps the record portable (no clock skew
    /// between recorder instance and consumer).
    pub ts_ms: u64,
    #[serde(flatten)]
    pub kind: TraceEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEventKind {
    PhaseStart { phase: String, turn: u32 },
    PhaseEnd { phase: String, turn: u32 },
    LlmCallStart {
        turn: u32,
        model: String,
    },
    LlmCallEnd {
        turn: u32,
        finish_reason: Option<String>,
    },
    ToolStart {
        tool: String,
        tool_call_id: String,
    },
    ToolEnd {
        tool: String,
        tool_call_id: String,
        ok: bool,
    },
    Error {
        message: String,
    },
}

/// Bounded LRU-ish in-memory per-run trace recorder.
///
/// Calls to `record` for an unknown `trace_id` are silently dropped; this
/// mirrors the existing WS entry path where a `trace_id` may already exist
/// in the wire envelope before the recorder is initialized.
pub struct RunTraceRecorder {
    inner: Mutex<RunTraceRecorderInner>,
}

struct RunTraceRecorderInner {
    traces: HashMap<String, RunTrace>,
    order: VecDeque<String>,
    max_traces: usize,
}

impl RunTraceRecorder {
    /// `max_traces` caps concurrent remembered runs. When full, the
    /// least-recently-`start()`ed run is evicted on the next `start()`.
    /// Must be `>= 1`.
    pub fn new(max_traces: usize) -> Self {
        Self {
            inner: Mutex::new(RunTraceRecorderInner {
                traces: HashMap::new(),
                order: VecDeque::new(),
                max_traces: max_traces.max(1),
            }),
        }
    }

    /// Register a new run. Idempotent: re-calling with the same `trace_id`
    /// is a no-op (preserves prior events and original start time).
    pub fn start(&self, trace_id: impl Into<String>) {
        let id = trace_id.into();
        let mut inner = self.inner.lock().expect("RunTraceRecorder mutex poisoned");
        if inner.traces.contains_key(&id) {
            return;
        }
        if inner.traces.len() >= inner.max_traces {
            if let Some(old) = inner.order.pop_front() {
                inner.traces.remove(&old);
            }
        }
        inner.traces.insert(
            id.clone(),
            RunTrace {
                trace_id: id.clone(),
                started_at_ms: now_ms(),
                events: Vec::new(),
            },
        );
        inner.order.push_back(id);
    }

    /// Append an event to an existing run. Silently no-op if the `trace_id`
    /// was never registered (callers can always `start()` defensively).
    pub fn record(&self, trace_id: &str, kind: TraceEventKind) {
        let mut inner = self.inner.lock().expect("RunTraceRecorder mutex poisoned");
        let started = match inner.traces.get(trace_id) {
            Some(t) => t.started_at_ms,
            None => return,
        };
        let entry = inner
            .traces
            .get_mut(trace_id)
            .expect("trace_id present from lookup above");
        let now = now_ms();
        entry.events.push(TraceEvent {
            ts_ms: now.saturating_sub(started),
            kind,
        });
    }

    /// Snapshot of one run. Returns `None` if the `trace_id` is unknown
    /// (e.g. evicted).
    pub fn snapshot(&self, trace_id: &str) -> Option<RunTrace> {
        self.inner
            .lock()
            .expect("RunTraceRecorder mutex poisoned")
            .traces
            .get(trace_id)
            .cloned()
    }

    /// List trace IDs currently held, most-recent first.
    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("RunTraceRecorder mutex poisoned")
            .order
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    /// Drop a single run explicitly. Returns true if the run was present.
    pub fn drop_trace(&self, trace_id: &str) -> bool {
        let mut inner = self.inner.lock().expect("RunTraceRecorder mutex poisoned");
        let removed = inner.traces.remove(trace_id).is_some();
        if removed {
            inner.order.retain(|id| id != trace_id);
        }
        removed
    }

    /// Current number of stored runs. Useful for tests + capacity tuning.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("RunTraceRecorder mutex poisoned")
            .traces
            .len()
    }

    /// Configured max capacity.
    pub fn capacity(&self) -> usize {
        self.inner
            .lock()
            .expect("RunTraceRecorder mutex poisoned")
            .max_traces
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_after_start_appends_in_order() {
        let r = RunTraceRecorder::new(8);
        r.start("t1");
        r.record(
            "t1",
            TraceEventKind::PhaseStart {
                phase: "Init".into(),
                turn: 0,
            },
        );
        r.record(
            "t1",
            TraceEventKind::LlmCallStart {
                turn: 0,
                model: "gpt-4o-mini".into(),
            },
        );
        let snap = r.snapshot("t1").expect("trace present");
        assert_eq!(snap.trace_id, "t1");
        assert_eq!(snap.events.len(), 2);
        // ts_ms is monotonic non-decreasing.
        assert!(snap.events[1].ts_ms >= snap.events[0].ts_ms);
        assert!(matches!(
            snap.events[0].kind,
            TraceEventKind::PhaseStart { ref phase, turn: 0 } if phase == "Init"
        ));
        assert!(matches!(
            snap.events[1].kind,
            TraceEventKind::LlmCallStart { ref model, .. } if model == "gpt-4o-mini"
        ));
    }

    #[test]
    fn record_for_unknown_trace_id_is_silently_dropped() {
        // Mirrors the wire-envelope entry: a trace_id may arrive via WS
        // before the recorder is started. record() must not panic.
        let r = RunTraceRecorder::new(4);
        r.record(
            "ghost",
            TraceEventKind::Error {
                message: "boom".into(),
            },
        );
        assert!(r.snapshot("ghost").is_none());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn start_is_idempotent_and_preserves_events() {
        let r = RunTraceRecorder::new(4);
        r.start("t1");
        r.record(
            "t1",
            TraceEventKind::Error {
                message: "kept".into(),
            },
        );
        r.start("t1");
        r.record(
            "t1",
            TraceEventKind::Error {
                message: "kept2".into(),
            },
        );
        let snap = r.snapshot("t1").unwrap();
        assert_eq!(snap.events.len(), 2, "second start must not reset");
        let t1_started = snap.started_at_ms;
        // Calling start again later should not advance started_at_ms.
        std::thread::sleep(std::time::Duration::from_millis(2));
        r.start("t1");
        assert_eq!(r.snapshot("t1").unwrap().started_at_ms, t1_started);
    }

    #[test]
    fn eviction_drops_oldest_when_capacity_exceeded() {
        let r = RunTraceRecorder::new(2);
        r.start("a");
        r.start("b");
        r.start("c");
        assert_eq!(r.len(), 2);
        assert!(r.snapshot("a").is_none(), "oldest must be evicted");
        assert!(r.snapshot("b").is_some());
        assert!(r.snapshot("c").is_some());
        // list() is most-recent first.
        assert_eq!(r.list(), vec!["c".to_string(), "b".to_string()]);
    }

    #[test]
    fn drop_trace_removes_only_the_named_run() {
        let r = RunTraceRecorder::new(4);
        r.start("a");
        r.start("b");
        assert!(r.drop_trace("a"));
        assert!(!r.drop_trace("a"), "double-drop returns false");
        assert!(r.snapshot("a").is_none());
        assert!(r.snapshot("b").is_some());
    }

    #[test]
    fn capacity_floor_is_at_least_one() {
        // Zero / negative shouldn't disable; treat as 1.
        let r = RunTraceRecorder::new(0);
        assert_eq!(r.capacity(), 1);
        r.start("a");
        r.start("b");
        assert_eq!(r.len(), 1);
    }
}
