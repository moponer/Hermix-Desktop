//! JSONL action trace for `hermix-cua`.
//!
//! Every action performed by the crate (after the safety guard
//! accepts it) is recorded as a JSON line to a session-scoped
//! file. The audit trail lets the desktop UI render an "action
//! history" panel, lets ops review what happened, and is the
//! input to a future "replay" feature.
//!
//! Tracing is **opt-in** — callers must construct a
//! [`TraceRecorder`] and pass it explicitly into the
//! capture/action entry points. When no recorder is configured,
//! the functions run with zero overhead (the recorder is
//! `Option<...>` in the call sites).

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CuaError;
use crate::CuaResult;

/// Kind of trace event. The struct form (`Other { subkind }`) is
/// used for events that don't fit the canonical list; serde keeps
/// it flat so JSONL consumers don't need to special-case nested
/// unions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceKind {
    /// `screenshot` finished.
    Screenshot,
    /// `screen_info` query finished.
    ScreenInfo,
    /// `left_click` / `right_click` etc.
    Click,
    /// `type_text` finished.
    TypeText,
    /// `press_key` / key chord finished.
    PressKey,
    /// Safety guard rejected an action.
    Denied,
    /// Action exceeded its time budget.
    Timeout,
    /// Anything not covered above (UIA element query, OCR run, …).
    Other { subkind: String },
}

impl TraceKind {
    pub fn as_str(&self) -> &str {
        match self {
            TraceKind::Screenshot => "screenshot",
            TraceKind::ScreenInfo => "screen_info",
            TraceKind::Click => "click",
            TraceKind::TypeText => "type_text",
            TraceKind::PressKey => "press_key",
            TraceKind::Denied => "denied",
            TraceKind::Timeout => "timeout",
            TraceKind::Other { subkind } => subkind,
        }
    }
}

/// One trace row. Serialised as a single JSON object per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// ISO-8601 UTC timestamp (`2026-06-23T00:01:23.456Z`).
    pub ts: DateTime<Utc>,
    pub kind: TraceKind,
    /// Stable identifier for the session that issued the action.
    /// Empty when no session context is available.
    pub session_id: String,
    /// Free-form description (e.g. coords for `click`, chord for
    /// `press_key`).
    pub summary: String,
    /// Whether the action succeeded.
    pub ok: bool,
    /// Wall-clock duration of the action.
    pub elapsed_ms: u64,
    /// Optional error message (present when `ok = false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TraceEvent {
    pub fn new(kind: TraceKind, session_id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ts: Utc::now(),
            kind,
            session_id: session_id.into(),
            summary: summary.into(),
            ok: true,
            elapsed_ms: 0,
            error: None,
        }
    }

    pub fn with_elapsed_ms(mut self, ms: u64) -> Self {
        self.elapsed_ms = ms;
        self
    }

    pub fn with_error(mut self, err: &CuaError) -> Self {
        self.ok = false;
        self.error = Some(err.to_string());
        self
    }

    pub fn render_json(&self) -> CuaResult<String> {
        serde_json::to_string(self)
            .map_err(|e| CuaError::InvalidParams(format!("trace json encode: {e}")))
    }
}

/// Append-only JSONL recorder. The file is opened lazily on the
/// first event and held open for the recorder's lifetime; flushing
/// is explicit via [`TraceRecorder::flush`] for callers that need
/// durability before a checkpoint.
pub struct TraceRecorder {
    path: Option<PathBuf>,
    writer: Mutex<Option<BufWriter<File>>>,
}

impl TraceRecorder {
    /// Construct a recorder that writes to `path`. The file is
    /// created if it does not exist; existing files are appended.
    pub fn open(path: impl AsRef<Path>) -> CuaResult<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| CuaError::InvalidParams(format!(
                "trace open {}: {e}", path.display()
            )))?;
        Ok(Self {
            path: Some(path),
            writer: Mutex::new(Some(BufWriter::new(file))),
        })
    }

    /// Construct a recorder that **discards** every event. Useful
    /// for tests and for callers that explicitly do not want an
    /// audit trail.
    pub fn null() -> Self {
        Self {
            path: None,
            writer: Mutex::new(None),
        }
    }

    pub fn is_active(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Write a single event. No-op when the recorder is the null
    /// sink.
    pub fn record(&self, event: &TraceEvent) -> CuaResult<()> {
        let line = event.render_json()?;
        let mut guard = self.writer.lock().expect("trace recorder mutex poisoned");
        if let Some(w) = guard.as_mut() {
            writeln!(w, "{line}")
                .map_err(|e| CuaError::InvalidParams(format!("trace write: {e}")))?;
        }
        Ok(())
    }

    /// Flush any buffered bytes to disk. No-op when the recorder
    /// is the null sink.
    pub fn flush(&self) -> CuaResult<()> {
        let mut guard = self.writer.lock().expect("trace recorder mutex poisoned");
        if let Some(w) = guard.as_mut() {
            w.flush()
                .map_err(|e| CuaError::InvalidParams(format!("trace flush: {e}")))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn recorder_writes_one_json_object_per_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cua-trace.jsonl");
        let r = TraceRecorder::open(&path).unwrap();
        r.record(&TraceEvent::new(
            TraceKind::Screenshot,
            "sess-1",
            "monitor=0 1920x1080",
        ))
        .unwrap();
        r.record(&TraceEvent::new(
            TraceKind::Click,
            "sess-1",
            "x=100 y=200 left",
        ))
        .unwrap();
        r.flush().unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line must be a parseable JSON object.
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("ts").is_some());
            assert!(v.get("kind").is_some());
        }
    }

    #[test]
    fn null_recorder_swallows_events() {
        let r = TraceRecorder::null();
        assert!(!r.is_active());
        r.record(&TraceEvent::new(TraceKind::Screenshot, "x", "y")).unwrap();
        r.flush().unwrap();
    }

    #[test]
    fn trace_event_with_error_round_trips() {
        let err = CuaError::Denied("test".into());
        let ev = TraceEvent::new(TraceKind::PressKey, "s", "ctrl+alt+delete")
            .with_elapsed_ms(5)
            .with_error(&err);
        let json = ev.render_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("test"));
    }

    #[test]
    fn trace_kind_other_serialises_subkind() {
        let kind = TraceKind::Other {
            subkind: "uia_query".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["kind"], "other");
        assert_eq!(v["subkind"], "uia_query");
    }
}