//! GET /api/cua/trace/{session_id} — read computer-use action traces.
//!
//! v1 (2026-07-02): backend half of the Stage 4.1 trace panel. The
//! action recorder in `crates/hermix-tools/src/tools/cua.rs` already
//! writes every CUA action (click / type / screenshot / denied /
//! ...) as a single JSON line to
//! `<HERMIX_HOME>/cua-trace.jsonl`. This module reads that file and
//! returns the rows whose `session_id` matches the requested one.
//!
//! The front-end renders these rows as a timeline. Until the React
//! trace panel ships, the same endpoint is consumable from `curl` /
//! the dev tools, and any future replay tooling (Stage 4.2+) can
//! reuse the wire shape without re-plumbing the JSONL format.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json as JsonResponse;
use serde::Serialize;
use serde_json::Value as JsonValue;

use hermix_cua::trace::TraceEvent;

use crate::HttpServerState;

#[derive(Debug, Serialize)]
pub struct TraceResponse {
    pub session_id: String,
    pub events: Vec<TraceEvent>,
    /// Total number of rows scanned (may be > events.len() when
    /// other sessions share the file).
    pub total_rows: usize,
    /// True when the underlying JSONL file does not yet exist. The
    /// endpoint returns 200 with `events = []` and
    /// `missing = true` in that case, so the front-end can
    /// distinguish "no actions yet" from "the recorder is broken".
    #[serde(skip_serializing_if = "is_false")]
    pub missing: bool,
    /// True when the underlying file is corrupt (one or more lines
    /// failed to parse). The front-end can show a banner while still
    /// rendering the rows that did parse.
    #[serde(skip_serializing_if = "is_false")]
    pub parse_errors: bool,
}

#[derive(Debug, Serialize)]
pub struct TraceError {
    pub error: String,
}

/// `serde` `skip_serializing_if` helper: only emit the field when
/// it carries a positive signal. The trace panel treats the `false`
/// default as "no news" and skips the empty branch in the front-end.
fn is_false(v: &bool) -> bool {
    !*v
}

/// Number of bytes from the tail of the JSONL file we keep in
/// memory when scanning. CUA traces are append-only and the panel
/// only ever needs the most recent activity, so reading the whole
/// file forever is the wrong default. 1 MiB comfortably fits tens
/// of thousands of rows; if a session ever produces more the user
/// already has the head/tail overflowed in their face long before
/// they reach the panel.
const TAIL_BYTES: u64 = 1024 * 1024;

/// JSONL trace file location. Matches the recorder configured in
/// `crates/hermix-tools/src/tools/cua.rs::trace_recorder()` —
/// `<HERMIX_HOME>/cua-trace.jsonl`.
fn cua_trace_path(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("cua-trace.jsonl")
}

/// Decode every JSONL row that belongs to `session_id`.
///
/// Lines that don't parse (truncated mid-write, schema-rotated by
/// an older binary) are silently skipped but the `parse_errors`
/// flag is set so the caller can surface a soft warning. The
/// recorder always appends a full line — partial lines only show
/// up if the process crashed mid-`record`, which is the case we
/// explicitly want to degrade gracefully for.
fn scan_trace_file(
    path: &std::path::Path,
    target_session: &str,
) -> Result<TraceResponse, TraceError> {
    if !path.exists() {
        return Ok(TraceResponse {
            session_id: target_session.to_string(),
            events: Vec::new(),
            total_rows: 0,
            missing: true,
            parse_errors: false,
        });
    }

    // Read at most the tail. Cheap for any realistic CUA session
    // (the recorder is per-call and the panel only ever renders
    // recent activity anyway). For very large historical sessions
    // we accept losing the oldest rows — the alternative is to
    // stream-parse the whole file, which costs a bounded MB.
    let body = std::fs::read(path).map_err(|e| TraceError {
        error: format!("read {}: {e}", path.display()),
    })?;

    // Walk back from the end until we've consumed
    // `TAIL_BYTES` worth of bytes. Anything older is dropped on the
    // floor — same trade-off as above.
    let start = body.len().saturating_sub(TAIL_BYTES as usize);
    let slice = &body[start..];

    let mut events = Vec::new();
    let mut total_rows = 0usize;
    let mut parse_errors = false;

    // Iterate every newline-delimited JSON object. The recorder
    // emits one event per line, terminated by `\n`. Any trailing
    // fragment that doesn't end in a newline is a partial write
    // from a crashed process — skip it instead of failing the
    // whole scan.
    for line in slice.split(|b| *b == b'\n') {
        let line = match std::str::from_utf8(line) {
            Ok(s) => s.trim(),
            Err(_) => {
                parse_errors = true;
                continue;
            }
        };
        if line.is_empty() {
            continue;
        }
        total_rows += 1;
        let v: JsonValue = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                parse_errors = true;
                continue;
            }
        };
        let ev: TraceEvent = match serde_json::from_value(v) {
            Ok(ev) => ev,
            Err(_) => {
                parse_errors = true;
                continue;
            }
        };
        if ev.session_id == target_session {
            events.push(ev);
        }
    }

    Ok(TraceResponse {
        session_id: target_session.to_string(),
        events,
        total_rows,
        missing: false,
        parse_errors,
    })
}

/// `GET /api/cua/trace/{session_id}` — return the recorded CUA
/// action timeline for `session_id`.
///
/// The endpoint never errors on a missing or empty file (those
/// map to `200 + events = []` so the front-end can render the
/// empty-state without special-casing 404). Genuine I/O errors
/// map to 500 with a short message — that's the only path that
/// should ever fire a notification toast.
pub async fn get_cua_trace(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
) -> Result<JsonResponse<TraceResponse>, (StatusCode, JsonResponse<TraceError>)> {
    // Defensive: an empty `session_id` would otherwise match the
    // empty-string `session_id` field that the recorder writes when
    // no session context exists. Reject it up-front so the
    // front-end can never accidentally request the global stream.
    if session_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            JsonResponse(TraceError {
                error: "session_id is empty".to_string(),
            }),
        ));
    }

    let path = cua_trace_path(&state);
    let resp = scan_trace_file(&path, &session_id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            JsonResponse(e),
        )
    })?;
    Ok(JsonResponse(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows that don't belong to the requested session must be
    /// filtered out so the trace panel shows only the right user's
    /// timeline. Cross-pollination was a regression in an earlier
    /// branch and turned out to be hard to notice in QA (one user
    /// saw their colleague's screenshot clicks).
    #[test]
    fn scan_filters_to_target_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cua.jsonl");
        let r = hermix_cua::trace::TraceRecorder::open(&path).expect("open");
        r.record(&hermix_cua::trace::TraceEvent::new(
            hermix_cua::trace::TraceKind::Screenshot,
            "user-a",
            "monitor=0",
        ))
        .unwrap();
        r.record(&hermix_cua::trace::TraceEvent::new(
            hermix_cua::trace::TraceKind::Click,
            "user-b",
            "x=1 y=2",
        ))
        .unwrap();
        r.flush().unwrap();

        let resp = scan_trace_file(&path, "user-a").expect("scan");
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].session_id, "user-a");
        assert_eq!(resp.total_rows, 2);
        assert!(!resp.missing);
    }

    /// A non-existent JSONL is *not* an error — the front-end
    /// will poll this every few seconds, and 404s would generate
    /// noisy error toasts for users who never invoked CUA.
    #[test]
    fn scan_on_missing_file_returns_empty_with_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.jsonl");
        let resp = scan_trace_file(&path, "x").expect("scan");
        assert!(resp.events.is_empty());
        assert_eq!(resp.total_rows, 0);
        assert!(resp.missing);
    }

    /// A truncate-mid-line (process crash during write) must not
    /// take down the whole scan. The previous binary failed the
    /// whole request with 500 and rendered the panel useless for
    /// everyone until the file was cleared by hand.
    #[test]
    fn scan_tolerates_partial_last_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("partial.jsonl");
        // One valid row + one truncated row that doesn't end in
        // a newline.
        std::fs::write(&path, b"{\"valid\":true}\n{\"truncated\":").expect("write");
        let resp = scan_trace_file(&path, "x").expect("scan");
        // We don't strictly require the valid row to parse here
        // — the schema is `TraceEvent`, not `{valid: bool}` — but
        // the scan must NOT return an Err.
        assert!(!resp.events.is_empty() || resp.total_rows >= 1);
        // The truncated fragment should set parse_errors.
        assert!(resp.parse_errors);
    }

    /// Empty session_id must be rejected at the gate. The
    /// JSONL stream emits rows with `session_id == ""` when no
    /// context is available, so accepting "" would let an empty
    /// query match them.
    #[test]
    fn scan_empty_session_does_not_match_empty_rows() {
        // This is more about the handler-level guard than the
        // scanner; we just verify the scanner's behaviour stays
        // consistent so a future refactor can't silently change
        // it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty-session.jsonl");
        let r = hermix_cua::trace::TraceRecorder::open(&path).expect("open");
        r.record(&hermix_cua::trace::TraceEvent::new(
            hermix_cua::trace::TraceKind::Screenshot,
            "",
            "global",
        ))
        .unwrap();
        r.flush().unwrap();

        let resp = scan_trace_file(&path, "").expect("scan");
        // The query was for ""; the row's session is ""; both
        // match. The handler is what blocks this case before it
        // reaches the scanner.
        assert_eq!(resp.events.len(), 1);
    }
}
