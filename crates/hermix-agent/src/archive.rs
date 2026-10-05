//! Short-term context unload ("archive").
//!
//! When the conversation grows past the 80% context-pressure threshold,
//! `auto_compress_if_over_threshold` calls
//! [`archive_context_section`] **before** the LLM-powered compressor
//! fires. The function:
//!
//! 1. Takes the *middle* of the message list (everything after the
//!    trailing system messages and before the last two messages,
//!    which are always the most recent user turn and assistant
//!    reply), serialises those messages to a Markdown file under
//!    `<hermix_home>/archive/YYYY-MM-DD/HHMMSS-<short-hash>.md`,
//! 2. Tags each message with an HTML anchor `<a id="node:...">` and a
//!    leading `<!-- node_id:... -->` comment so the model (or a human
//!    debugging) can hop directly to a specific message,
//! 3. Returns the [`ArchiveRef`] list to the caller so it can be
//!    surfaced back to the frontend and rendered as a hover-link in
//!    the chat UI (see TencentDB Agent Memory借鉴 P0: refs/*.md).
//!
//! Design choice (用户 02:35 "按最优方案先实现"):
//! - We deliberately do **not** add a `MemoryLayer::archive_section`
//!   trait method in v2. Archive is a local-only behaviour — the 8
//!   external HTTPS plugins don't need (and shouldn't take on) a
//!   write surface for evidence-preservation. The v1 trait remains
//!   stable; P1 trait addition can be a follow-up if telemetry
//!   shows a backpressure need.
//! - We deliberately do **not** emit Mermaid diagrams here. v2 is
//!   about *where the evidence lives and how to reference it*.
//!   Rendering is the front-end's job (currently a follow-up).
//!
//! Complete-implementation iron rule: every helper in this module has
//! at least one caller in the agent loop; nothing here is reserved
//! "for later" without a follow-up issue.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use hermix_core::{Message, MessageRole};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One message that has been archived to disk.
/// Returned to the caller so the frontend can render a "see archived
/// context" affordance and so the agent loop can stuff the list into
/// a follow-up system-prompt block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveRef {
    /// 8-hex-char stable id derived from `timestamp + index + content`.
    pub node_id: String,
    /// Absolute path to the file holding this node (also where the
    /// `node_id` anchor lives).
    pub file_path: PathBuf,
    /// Inclusive 1-based line range inside `file_path`. `None` if the
    /// file could not be parsed back (the caller should still surface
    /// `node_id` and `file_path` even when the line range is absent).
    pub line_range: Option<(usize, usize)>,
    /// Character count of the archived message content (used for the
    /// "pressure" badge).
    pub char_count: usize,
}

/// Errors that can happen while archiving a section.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("io error writing archive file: {0}")]
    Io(#[from] io::Error),
    #[error("could not resolve archive directory: {0}")]
    BadPath(String),
    #[error("refusing to archive empty section")]
    Empty,
}

/// Stable 8-hex-char node id. We use `md5` because it's already in the
/// agent's dep tree (and we only need 32 bits of collision resistance
/// per archive file — the timestamp + index disambiguates cross-file).
fn make_node_id(timestamp: &DateTime<Local>, idx: usize, content: &str) -> String {
    let head: String = content.chars().take(50).collect();
    let payload = format!("{}|{idx}|{head}", timestamp.timestamp_millis());
    let digest = md5::compute(payload.as_bytes());
    let hex = format!("{:x}", digest);
    hex.chars().take(8).collect()
}

/// Pick the *middle* slice of `messages` that should be archived.
/// Rules:
/// - System messages at the head (if any) stay in `ctx`.
/// - The last two messages (most recent user turn + assistant reply)
/// stay in `ctx` so the LLM still has the immediate context.
/// - Everything in between is fair game for archiving.
fn pick_archivable(messages: &[Message]) -> std::ops::Range<usize> {
    let system_end = messages
        .iter()
        .take_while(|m| m.role == MessageRole::System)
        .count();
    if messages.len() <= system_end + 2 {
        0..0
    } else {
        system_end..messages.len().saturating_sub(2)
    }
}

/// Render a single message into Markdown with a leading anchor.
/// Format example:
/// ```text
/// ## Message 3
/// <!-- node_id: a3b9c1f0 -->
/// <a id="node:a3b9c1f0"></a>
/// **user**: Please list the failing tests.
/// ```
fn render_message_md(idx: usize, node_id: &str, m: &Message) -> String {
    let role = match m.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::Automation => "automation",
    };
    let body = m.content.as_deref().unwrap_or("");
    let mut out = String::with_capacity(body.len() + 96);
    writeln!(out, "## Message {idx}").expect("writing to String never fails");
    writeln!(out, "<!-- node_id: {node_id} -->").expect("writing to String never fails");
    writeln!(out, "<a id=\"node:{node_id}\"></a>").expect("writing to String never fails");
    writeln!(out, "**{role}**: {body}").expect("writing to String never fails");
    out
}

/// Build the file-level header (timestamps, totals, Mermaid stub).
fn render_header(timestamp: &DateTime<Local>, total_chars: usize, message_count: usize) -> String {
    format!(
        "# Archived context section ({ts})\n\
         <!-- archive_meta: char_count={chars} message_count={count} -->\n\
         <!-- node_id_prefix: {prefix} -->\n\
         <!-- to_reference: append `#node:<node_id>` to this file's URL -->\n\n",
        ts = timestamp.format("%Y-%m-%dT%H:%M:%S"),
        chars = total_chars,
        count = message_count,
        prefix = timestamp.format("%Y%m%d%H%M%S"),
    )
}

/// Compute the archive directory for `now` under `hermix_home`:
/// `<hermix_home>/archive/YYYY-MM-DD`.
fn archive_dir_for(hermix_home: &Path, now: &DateTime<Local>) -> PathBuf {
    hermix_home
        .join("archive")
        .join(now.format("%Y-%m-%d").to_string())
}

/// Per-session index file path: `<hermix_home>/sessions/<sid>/archive-index.jsonl`.
/// The directory layout deliberately matches the existing markdown
/// session log (`<hermix_home>/sessions/<sid>/*.md`) so the user can
/// `ls sessions/<sid>/` and find both the readable log and the
/// machine-readable archive index in one place. The index is a
/// JSON-Lines file (one `ArchiveRef` per line) — append-only, easy
/// to grep, easy to tail, easy to back up. The front-end endpoint
/// reads the whole file each call; for a desktop session this is
/// O(hundreds of entries) which is fine.
fn session_index_path(hermix_home: &Path, session_id: &str) -> PathBuf {
    hermix_home
        .join("sessions")
        .join(session_id)
        .join("archive-index.jsonl")
}

/// Append `refs` to the per-session index file. **Best-effort**:
/// logs a warning on failure but never propagates the error — the
/// archive file itself is already on disk, the index is just a
/// lookup table the UI uses to surface refs.
fn append_session_index(
    hermix_home: &Path,
    session_id: &str,
    refs: &[ArchiveRef],
) {
    if session_id.is_empty() || refs.is_empty() {
        return;
    }
    let path = session_index_path(hermix_home, session_id);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!(
                "archive: failed to create session index dir {}: {e}",
                parent.display()
            );
            return;
        }
    }
    let mut buf = String::with_capacity(refs.len() * 128);
    for r in refs {
        match serde_json::to_string(r) {
            Ok(line) => {
                buf.push_str(&line);
                buf.push('\n');
            }
            Err(e) => {
                tracing::warn!(
                    "archive: failed to serialize ref for session {session_id}: {e}"
                );
            }
        }
    }
    if let Err(e) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, buf.as_bytes()))
    {
        tracing::warn!(
            "archive: failed to append to session index {}: {e}",
            path.display()
        );
    }
}

/// Append rolled-up tool-result originals to a per-session evidence log
/// (`<hermix_home>/sessions/<sid>/tool-rollup-archive.md`) so the
/// continuous `rollup_tool_results` drain never destroys recoverable
/// output — the one-line summaries living in ctx stay backed by the
/// full text on disk. **Best-effort**: failures only log, never
/// propagate — rolling up context pressure is never worth failing a
/// turn over.
pub fn append_rollup_archive(
    hermix_home: &Path,
    session_id: Option<&str>,
    originals: &[(usize, String)],
) {
    let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
        return;
    };
    if originals.is_empty() {
        return;
    }
    let path = hermix_home.join("sessions").join(sid).join("tool-rollup-archive.md");
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!(
                "rollup archive: failed to create dir {}: {e}",
                parent.display()
            );
            return;
        }
    }
    let now = Local::now();
    let mut buf = String::with_capacity(128 + originals.iter().map(|(_, c)| c.len() + 96).sum::<usize>());
    for (idx, content) in originals {
        buf.push_str(&format!(
            "\n## {} — message #{} ({} chars)\n\n```\n{}\n```\n",
            now.format("%Y-%m-%d %H:%M:%S"),
            idx,
            content.len(),
            content
        ));
    }
    if let Err(e) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, buf.as_bytes()))
    {
        tracing::warn!(
            "rollup archive: failed to append to {}: {e}",
            path.display()
        );
    }
}

/// Read all archived refs for a session from its index file. Returns
/// an empty `Vec` when the session has never been archived (the
/// common case for short sessions).
/// Used by the hermix-server `GET /v1/sessions/{id}/archive-refs`
/// endpoint. Malformed lines are skipped (best-effort) — a single
/// corrupt line should not take the whole UI down.
pub fn read_session_index(
    hermix_home: &Path,
    session_id: &str,
) -> Vec<ArchiveRef> {
    if session_id.is_empty() {
        return Vec::new();
    }
    let path = session_index_path(hermix_home, session_id);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ArchiveRef>(line) {
            Ok(r) => out.push(r),
            Err(e) => tracing::warn!(
                "archive: skipping malformed session-index line {}: {e}",
                lineno + 1
            ),
        }
    }
    out
}

/// Write the section to disk and return a [`ArchiveRef`] per archived
/// message.
/// On success the returned refs always carry `file_path` and
/// `node_id`; `line_range` is filled in by re-scanning the file we
/// just wrote so the frontend can jump to a precise line.
/// `session_id` is optional — when `Some`, refs are also appended to
/// `<hermix_home>/sessions/<sid>/archive-index.jsonl` so the UI can
/// list them later without re-scanning every archive file. When
/// `None`, the archive file is still written but no index is kept
/// (useful for tests and for the legacy no-session path).
pub fn archive_context_section(
    messages: &[Message],
    hermix_home: &Path,
    now: Option<DateTime<Local>>,
    session_id: Option<&str>,
) -> Result<Vec<ArchiveRef>, ArchiveError> {
    let slice = pick_archivable(messages);
    if slice.is_empty() {
        return Err(ArchiveError::Empty);
    }
    let now = now.unwrap_or_else(Local::now);
    let slice = &messages[slice];
    let total_chars: usize = slice
        .iter()
        .map(|m| m.content.as_deref().map(str::len).unwrap_or(0))
        .sum();

    let dir = archive_dir_for(hermix_home, &now);
    fs::create_dir_all(&dir).map_err(|e| {
        ArchiveError::BadPath(format!("{}: {e}", dir.display()))
    })?;

    let stamp = now.format("%H%M%S").to_string();
    // Short file hash to avoid clobber when two archives fire in the
    // same second. 6 hex chars = 16M possibilities, plenty for a
    // desktop agent loop.
    let file_hash: String = format!(
        "{:x}",
        md5::compute(format!("{}|{}|{}", now.timestamp_millis(), slice.len(), total_chars).as_bytes())
    )
    .chars()
    .take(6)
    .collect();
    let file_path = dir.join(format!("{stamp}-{file_hash}.md"));

    let mut body = render_header(&now, total_chars, slice.len());
    let mut refs = Vec::with_capacity(slice.len());
    for (idx, m) in slice.iter().enumerate() {
        let node_id = make_node_id(&now, idx, m.content.as_deref().unwrap_or(""));
        let rendered = render_message_md(idx, &node_id, m);
        refs.push(ArchiveRef {
            node_id: node_id.clone(),
            file_path: file_path.clone(),
            line_range: None, // filled below
            char_count: m.content.as_deref().map(str::len).unwrap_or(0),
        });
        body.push_str(&rendered);
    }

    fs::write(&file_path, body.as_bytes())?;

    // Re-scan to populate precise line ranges for each node anchor.
    let raw = fs::read_to_string(&file_path)?;
    let mut ranges: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for (i, line) in raw.lines().enumerate() {
        if let Some(rest) = line.strip_prefix("<a id=\"node:") {
            if let Some(node) = rest.strip_suffix("\"></a>") {
                ranges
                    .entry(node.to_string())
                    .and_modify(|r| r.1 = i + 1)
                    .or_insert((i + 1, i + 1));
            }
        }
    }
    for r in &mut refs {
        if let Some((start, end)) = ranges.get(&r.node_id) {
            r.line_range = Some((*start, *end));
        }
    }

    // Best-effort: append to the session-level index. Failure here
    // must NOT fail the archive — the file is the source of truth,
    // the index is just a convenience for the UI.
    if let Some(sid) = session_id {
        append_session_index(hermix_home, sid, &refs);
    }

    Ok(refs)
}

/// Render the list of refs as a Markdown block the LLM can quote.
/// Output example:
/// ```text
/// ## Archived context
/// 12 messages totalling 4 567 chars have been archived to disk for
/// this session. To inspect a specific turn, reference one of the
/// following anchors (anchors resolve when this Markdown is rendered
/// alongside the archive file):
/// - node:`a3b9c1f0` — user (321 chars) — `<archive_dir>/<file>.md#L42`
/// - node:`5d4e2a87` — assistant (812 chars) — `<archive_dir>/<file>.md#L48`
/// ...
/// ```
pub fn render_archive_summary(refs: &[ArchiveRef]) -> String {
    if refs.is_empty() {
        return String::new();
    }
    let total_chars: usize = refs.iter().map(|r| r.char_count).sum();
    let mut out = String::with_capacity(256 + refs.len() * 80);
    out.push_str("## Archived context (load on demand)\n");
    let _ = writeln!(
        out,
        "{n} messages totalling {c} chars have been archived to disk. \
         When the user references something from earlier in this session, \
         mention the relevant `node_id` so the UI can offer to open the \
         archive file at that anchor.",
        n = refs.len(),
        c = total_chars,
    );
    out.push_str("\n");
    for r in refs {
        let path = r.file_path.display();
        let line_hint = match r.line_range {
            Some((s, e)) if s == e => format!("{path}#L{s}"),
            Some((s, e)) => format!("{path}#L{s}-L{e}"),
            None => path.to_string(),
        };
        let _ = writeln!(
            out,
            "- node:`{nid}` ({c} chars) — {line_hint}",
            nid = r.node_id,
            c = r.char_count,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    fn fixture_messages(n: usize) -> Vec<Message> {
        let mut v = Vec::with_capacity(n + 2);
        v.push(Message::system("You are Hermix."));
        for i in 0..n {
            v.push(Message::user(format!("Question #{i}")));
            v.push(Message::assistant(format!("Answer #{i}")));
        }
        v.push(Message::user("Latest question".to_string()));
        v.push(Message::assistant("Latest answer".to_string()));
        v
    }

    #[test]
    fn archive_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let refs =
            archive_context_section(&fixture_messages(5), tmp.path(), None, None).unwrap();
        assert!(!refs.is_empty(), "expected refs for 10 archivable messages");
        // Every ref points to a file under <tmp>/archive/<YYYY-MM-DD>/
        for r in &refs {
            assert!(r.file_path.starts_with(tmp.path()));
            assert!(r.file_path.exists());
        }
    }

    #[test]
    fn node_id_is_stable_for_same_input() {
        let now = Local::now();
        let id_a = make_node_id(&now, 3, "hello world");
        let id_b = make_node_id(&now, 3, "hello world");
        assert_eq!(id_a, id_b, "same input must yield same node_id");
        assert_eq!(id_a.len(), 8, "node_id is 8 hex chars");
        let id_c = make_node_id(&now, 4, "hello world");
        assert_ne!(id_a, id_c, "different index must change node_id");
    }

    #[test]
    fn archive_returns_refs_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let msgs = fixture_messages(3);
        let refs = archive_context_section(&msgs, tmp.path(), None, None).unwrap();
        // fixture_messages(3) = 1 system + 3*(user+assistant) + tail(user+assistant)
        // → archivable = messages[1..len-2] = 6 messages
        assert_eq!(refs.len(), 6);
        // Header is 4 comment lines + 1 blank = 5 lines. Each message
        // then occupies 4 lines (## heading, node_id comment,
        // <a id="..."> anchor, **role**: body). The anchor for
        // message i lives at 1-based line `8 + i*4`.
        for (i, r) in refs.iter().enumerate() {
            let expected_line = 8 + i * 4;
            assert_eq!(
                r.line_range,
                Some((expected_line, expected_line)),
                "anchor line for message {i} should be {expected_line}"
            );
        }
    }

    #[test]
    fn summary_mentions_node_ids_and_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let refs = archive_context_section(&fixture_messages(2), tmp.path(), None, None).unwrap();
        let summary = render_archive_summary(&refs);
        assert!(summary.contains("Archived context"));
        assert!(summary.contains(&refs[0].node_id));
        assert!(summary.contains(refs[0].file_path.to_str().unwrap()));
    }

    #[test]
    fn pick_archivable_reserves_system_and_tail() {
        let msgs = fixture_messages(2);
        let r = pick_archivable(&msgs);
        // fixture_messages(2) = 1 system + 4 + tail(2) = 7 messages
        assert_eq!(r, 1..5, "system + tail must not be archived");
    }

    #[test]
    fn archive_returns_empty_when_too_short() {
        let tmp = tempfile::tempdir().unwrap();
        // system + tail only → nothing archivable
        let msgs = vec![
            Message::system("sys"),
            Message::user("latest question"),
            Message::assistant("latest answer"),
        ];
        let err = archive_context_section(&msgs, tmp.path(), None, None).unwrap_err();
        assert!(matches!(err, ArchiveError::Empty));
    }

    #[test]
    fn archive_summary_empty_for_empty_refs() {
        assert!(render_archive_summary(&[]).is_empty());
    }

    #[test]
    fn session_index_writes_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "test-session-abc123";
        let refs =
            archive_context_section(&fixture_messages(3), tmp.path(), None, Some(sid)).unwrap();
        assert_eq!(refs.len(), 6, "fixture_messages(3) yields 6 archivable rows");

        // Index file should now exist on disk.
        let index_path = session_index_path(tmp.path(), sid);
        assert!(index_path.exists(), "session index must be created");
        let raw = std::fs::read_to_string(&index_path).unwrap();
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), refs.len(), "one JSONL line per ref");

        // read_session_index parses them all back losslessly.
        let round_tripped = read_session_index(tmp.path(), sid);
        assert_eq!(round_tripped.len(), refs.len());
        for (orig, rt) in refs.iter().zip(round_tripped.iter()) {
            assert_eq!(orig.node_id, rt.node_id);
            assert_eq!(orig.file_path, rt.file_path);
            assert_eq!(orig.char_count, rt.char_count);
            assert_eq!(orig.line_range, rt.line_range);
        }
    }

    #[test]
    fn session_index_empty_for_unknown_session() {
        let tmp = tempfile::tempdir().unwrap();
        let refs = read_session_index(tmp.path(), "never-archived");
        assert!(
            refs.is_empty(),
            "missing index file must return empty Vec (not an error)"
        );
    }

    #[test]
    fn archive_without_session_id_skips_index() {
        let tmp = tempfile::tempdir().unwrap();
        // None session_id → archive succeeds but no <tmp>/sessions/ dir
        // is touched.
        let refs =
            archive_context_section(&fixture_messages(2), tmp.path(), None, None).unwrap();
        assert!(!refs.is_empty());
        assert!(
            !tmp.path().join("sessions").exists(),
            "no session_id must not create a sessions/ subtree"
        );
    }
}
