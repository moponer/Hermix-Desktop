//! Message/approval/compression-archive read-write paths.
use std::path::{Path, PathBuf};

use chrono::Utc;
use hermix_core::{AgentError, AutomationMeta, Message, MessageRole};

use super::*;

/// How many leading chars of a message body take part in the mirror
/// fingerprint. Long enough to distinguish consecutive turns, short enough
/// that a tool result never has to be fully re-read to compare.
const FINGERPRINT_PREFIX_CHARS: usize = 96;

fn mirror_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::Automation => "automation",
    }
}

/// Identity of a live message for mirror resumption.
///
/// Prefix + char length + call id — not a hash: collisions produce a wrong
/// resume point, and a hash buys nothing here (the comparison is one line).
fn message_fingerprint(msg: &Message) -> String {
    let content = msg.content.as_deref().unwrap_or("");
    fingerprint_parts(
        mirror_role(msg.role),
        msg.tool_call_id.as_deref().unwrap_or(""),
        content,
    )
}

fn fingerprint_parts(role: &str, tool_call_id: &str, content: &str) -> String {
    let prefix: String = content.chars().take(FINGERPRINT_PREFIX_CHARS).collect();
    format!("{role}|{}|{tool_call_id}|{prefix}", content.chars().count())
}

/// Same fingerprint, computed from one already-serialized mirror line.
fn line_fingerprint(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(fingerprint_parts(
        v.get("role").and_then(|r| r.as_str())?,
        v.get("tool_call_id").and_then(|t| t.as_str()).unwrap_or(""),
        v.get("content").and_then(|c| c.as_str()).unwrap_or(""),
    ))
}

/// Last complete line of a JSONL file, dropping a torn tail first.
///
/// A process killed mid-`writeln!` leaves a fragment without its trailing
/// newline. Left in place it corrupts every later append (and any reader that
/// counts lines), so it is truncated away here — the fragment is by
/// construction a message the DB still owns.
///
/// Only a 16 KB tail window is read: the anchor is the last line, so the
/// front of the file is irrelevant (this was a full `read_to_string` per turn).
fn read_last_complete_line(path: &Path) -> std::io::Result<Option<String>> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    const TAIL_PROBE: u64 = 16 * 1024;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    let probe = TAIL_PROBE.min(len);
    f.seek(SeekFrom::Start(len - probe))?;
    let mut buf = Vec::with_capacity(probe as usize);
    f.read_to_end(&mut buf)?;

    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        // No newline in the window. If the whole file is one fragment, drop it;
        // if the file is simply longer than the window, the last line is huge
        // and cannot be anchored — the caller falls back to a full rewrite.
        if len <= probe {
            let _ = std::fs::OpenOptions::new().write(true).open(path)?.set_len(0);
        }
        return Ok(None);
    };

    // Torn tail → cut it off so appends resume at a line boundary.
    let keep = len - probe + last_nl as u64 + 1;
    if keep < len {
        std::fs::OpenOptions::new().write(true).open(path)?.set_len(keep)?;
    }

    let line_start = buf[..last_nl]
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line = String::from_utf8_lossy(&buf[line_start..last_nl])
        .trim_end()
        .to_string();
    Ok(if line.is_empty() { None } else { Some(line) })
}

/// One stored row, reduced to the columns the incremental flush compares.
///
/// Deliberately not `SELECT *`: the presentation columns
/// (`execution_timeline`, `unified_steps`, `extra`, `reasoning_content`) never
/// take part in "is this still the same message?", so reading them back every
/// turn would be pure cost.
struct StoredMessageRow {
    id: i64,
    role: String,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
}

impl StoredMessageRow {
    /// `true` when the stored row still represents this in-memory message.
    ///
    /// Only the fields that identify a turn are compared. A `NULL`/empty
    /// mismatch is treated as "different" — the safe direction, because it
    /// costs a re-insert rather than silently keeping a stale row.
    fn same_as(&self, msg: &Message) -> bool {
        if self.role != mirror_role(msg.role) {
            return false;
        }
        if self.content.as_deref() != msg.content.as_deref() {
            return false;
        }
        if self.tool_call_id.as_deref() != msg.tool_call_id.as_deref() {
            return false;
        }
        match (self.tool_calls.as_deref(), msg.tool_calls.as_ref()) {
            (None, None) => true,
            (Some(stored), Some(calls)) => {
                serde_json::to_string(calls).is_ok_and(|s| s == stored)
            }
            _ => false,
        }
    }
}

/// One timeline row at `offset` (0-based, id order), or `None`.
///
/// Used by the fast path to verify the two ends of the stored timeline without
/// reading all of it.
fn stored_row_at(
    conn: &rusqlite::Connection,
    session_id: &str,
    offset: usize,
) -> Option<StoredMessageRow> {
    conn.query_row(
        "SELECT id, role, content, tool_call_id, tool_calls FROM messages \
         WHERE session_id = ?1 AND role NOT IN ('automation', 'approval') \
         ORDER BY id ASC LIMIT 1 OFFSET ?2",
        rusqlite::params![session_id, offset as i64],
        |row| {
            Ok(StoredMessageRow {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                tool_call_id: row.get(3)?,
                tool_calls: row.get(4)?,
            })
        },
    )
    .ok()
}

/// Timeline rows for a session, in id order, excluding the append-only
/// `automation` side channel.
fn stored_message_rows(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Vec<StoredMessageRow>, AgentError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, role, content, tool_call_id, tool_calls \
             FROM messages \
             WHERE session_id = ?1 AND role NOT IN ('automation', 'approval') \
             ORDER BY id ASC",
        )
        .map_err(|e| AgentError::Io(format!("Failed to prepare message scan: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(StoredMessageRow {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                tool_call_id: row.get(3)?,
                tool_calls: row.get(4)?,
            })
        })
        .map_err(|e| AgentError::Io(format!("Failed to scan messages: {e}")))?;
    rows.collect::<Result<Vec<_>, rusqlite::Error>>()
        .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))
}

impl SessionPersistence {
    /// Persist the session's message timeline, incrementally.
    ///
    /// **Transaction-protected**: the caller (`save_session_atomic`) wraps
    /// this in `BEGIN IMMEDIATE` / `COMMIT`.
    ///
    /// ## Incremental, not rewrite
    ///
    /// The old implementation deleted every non-`automation` row and
    /// re-inserted the whole timeline on every turn. That is O(n) rows per
    /// turn, each firing two FTS5 triggers (6 triggers in total), so a
    /// 500-message session rewrote ~3000 index entries every turn and total
    /// IO grew O(n²) over a conversation. This is the main reason long
    /// sessions get progressively slower.
    ///
    /// Turns are append-only in the common case, so the right unit of work is
    /// "the rows that changed". This computes the **common prefix** between
    /// the stored rows and the in-memory timeline and rewrites only the tail:
    ///
    /// - steady state → prefix covers the whole history, tail = this turn
    ///   (2–6 rows);
    /// - compression → the prefix stops where the folded turns began, the
    ///   remnants are deleted and the summary + kept tail re-inserted;
    /// - `/undo` → the in-memory timeline is shorter, the undone rows fall out
    ///   of the prefix and are deleted, exactly as the blanket DELETE did.
    ///
    /// Rows inside the prefix keep their `id`. That matters because the wire
    /// id the front-end keys on is positional (`{session_id}-{idx}`): keeping
    /// the prefix in place keeps the order — and therefore those ids — stable.
    /// It also means a client-written `unified_steps` blob on a prefix row is
    /// never clobbered, which is why the old ordinal carry-forward hack is
    /// gone rather than ported.
    ///
    /// `automation` rows stay exempt (never compared, never deleted, always
    /// appended) exactly as before: they arrive via a separate delivery path
    /// and never round-trip through `load_session`.
    ///
    /// Returns the number of timeline rows the session now has, which
    /// `save_session_atomic` stamps onto `sessions.message_count`.
    pub(crate) fn flush_messages(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        messages: &[Message],
        now: &str,
    ) -> Result<usize, AgentError> {
        // Messages that occupy a tracked DB row, in timeline order.
        // `automation` is excluded — those rows are an append-only side
        // channel, so including them would break the prefix on the first
        // automation delivery of every session.
        let tracked: Vec<&Message> = messages
            .iter()
            .filter(|m| {
                !m.ephemeral
                    && !matches!(m.role, MessageRole::System | MessageRole::Automation)
            })
            .collect();

        // ## Fast path
        //
        // The previous flush stamped `sessions.message_count` with the number
        // of timeline rows it wrote. If that count still describes the stored
        // rows and the last of them is still the same message, then the stored
        // timeline is a verbatim prefix of what we are about to write and
        // nothing needs trimming — no need to read the history back at all.
        //
        // This is what makes the steady state O(1) in IO instead of O(n): on a
        // 500-message session the slow path still reads ~1 MB of content every
        // turn, which is the half of the old cost that deleting doesn't fix.
        //
        // Everything the fast path cannot prove falls through to the full scan
        // below, which is always correct on its own.
        //
        // The invariant it relies on: **between flushes the timeline only
        // changes by truncation or by rewriting from a prefix**. Every
        // transformation the loop applies — compression folding, transient
        // nudges stripped, adjacent assistant messages merged, `/undo` — changes
        // the number of tracked messages, so the count check catches it. A
        // middle row rewritten in place with the same neighbours and the same
        // total would slip through; nothing in the loop does that today.
        let mut prefix = 0usize;
        let mut scanned = false;
        let watermark: i64 = conn
            .query_row(
                "SELECT message_count FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if watermark > 0 && (watermark as usize) <= tracked.len() {
            let stored_total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages \
                     WHERE session_id = ?1 AND role NOT IN ('automation', 'approval')",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .unwrap_or(-1);
            // Three anchors — head, middle, tail — instead of the count alone.
            // A count match is not enough: compression can shrink the timeline
            // and later turns can grow it back to the same length, in which
            // case the stored middle would be stale while both ends still
            // match (compression always preserves the first messages and the
            // current last one). Sampling the middle catches exactly that.
            let w = watermark as usize;
            let anchors = if w >= 3 { [0, w / 2, w - 1] } else { [0, 0, w - 1] };
            let anchors_ok = anchors
                .iter()
                .all(|&off| stored_row_at(conn, session_id, off).is_some_and(|row| row.same_as(tracked[off])));
            if stored_total == watermark && anchors_ok {
                prefix = w;
            } else {
                scanned = true;
            }
        } else {
            scanned = true;
        }

        if scanned {
            let stored = stored_message_rows(conn, session_id)?;
            prefix = stored
                .iter()
                .zip(tracked.iter())
                .take_while(|(row, msg)| row.same_as(msg))
                .count();
            // Drop only the stored tail that no longer matches. Prefix rows are
            // left untouched — no DELETE, no re-INSERT, no FTS churn.
            //
            // `approval` rows are exempt alongside `automation`: they are
            // written by a separate INSERT for the UI timeline and never
            // round-trip through `load_session`, so they must neither take
            // part in the prefix comparison nor be deleted here. (The old
            // blanket DELETE did destroy them; the doc comment claimed they
            // survived — the claim was wrong until this filter existed.)
            if prefix < stored.len() {
                conn.execute(
                    "DELETE FROM messages \
                     WHERE session_id = ?1 \
                       AND role NOT IN ('automation', 'approval') AND id >= ?2",
                    rusqlite::params![session_id, stored[prefix].id],
                )
                .map_err(|e| AgentError::Io(format!("Failed to trim changed messages: {e}")))?;
            }
        }

        let mut stmt = conn
            .prepare(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, name, created_at, execution_timeline, reasoning_content, unified_steps, extra)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare insert: {e}")))?;

        let mut tracked_idx: usize = 0;
        for msg in messages {
            // ephemeral messages (e.g. replay-path
            // tool_result) exist only in the in-memory context and
            // must never be persisted to the DB.
            if msg.ephemeral {
                continue;
            }
            if matches!(msg.role, MessageRole::System) {
                continue;
            }
            // A row already present at this position keeps its identity:
            // skipping it here is what makes the write incremental, and what
            // preserves both its `id` (positional wire id downstream) and any
            // client-written `unified_steps`.
            if !matches!(msg.role, MessageRole::Automation) {
                let ordinal = tracked_idx;
                tracked_idx += 1;
                if ordinal < prefix {
                    continue;
                }
            }
            let role = match msg.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                // Persisted on purpose. `load_session` filters this role
                // back out of the LLM payload, which is exactly the
                // "stored and rendered but never sent to the model"
                // behaviour we need — the same split `approval` rows
                // already have.
                MessageRole::Automation => "automation",
            };

            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default());

            let timeline_json = msg
                .execution_timeline
                .as_ref()
                .map(|tl| serde_json::to_string(tl).unwrap_or_default());
            // No carry-forward needed any more: rows inside the prefix are
            // never rewritten, so a client-persisted `unified_steps` stays on
            // its row. Rows past the prefix are being (re)created, and the
            // in-memory value is the only truth for them.
            let unified_steps_json = msg
                .unified_steps
                .as_ref()
                .map(|us| serde_json::to_string(us).unwrap_or_default());

            let safe_content = msg
                .content
                .as_deref()
                .map(crate::pii_redact::redact_secrets);
            let safe_tool_calls = tool_calls_json
                .as_deref()
                .map(crate::pii_redact::redact_tool_calls_json);

            // Automation provenance rides in `extra` — the same column
            // approval cards use, and for the same reason: it is UI-only
            // data that no LLM payload should ever carry. Serialization
            // failure degrades to `NULL` rather than failing the flush:
            // the result text is the part that must not be lost, and a
            // nameless task header is still a truthful one.
            let extra_json = msg
                .automation_meta
                .as_ref()
                .and_then(|m| serde_json::to_string(m).ok());

            stmt.execute(rusqlite::params![
                session_id,
                role,
                safe_content.as_deref(),
                msg.tool_call_id.as_deref(),
                safe_tool_calls.as_deref(),
                msg.name.as_deref(),
                now,
                timeline_json.as_deref(),
                msg.reasoning_content.as_deref(),
                unified_steps_json.as_deref(),
                extra_json.as_deref(),
            ])
            .map_err(|e| AgentError::Io(format!("Failed to insert message: {e}")))?;
        }

        Ok(tracked.len())
    }

    /// Persist the desktop client's frontend-built `unified_steps`
    /// timeline onto the most recent assistant message of a session.
    ///
    /// The agent loop never produces `unified_steps` (it's assembled
    /// live in the desktop client from the WS stream), so it isn't
    /// part of the in-memory `Message` the loop flushes. The client
    /// calls this right after a turn's stream completes, shipping the
    /// exact chronological reasoning/tools/text steps it rendered. We
    /// attach them to the latest assistant row so a reload restores the
    /// same inline tool interleaving (instead of the fixed
    /// `buildFallbackSteps` order). `flush_messages`' carry-forward
    /// keeps this value alive across the next turn's DELETE+INSERT.
    pub fn update_latest_assistant_unified_steps(
        &self,
        session_id: &str,
        unified_steps_json: &str,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "UPDATE messages SET unified_steps = ?1 \
             WHERE session_id = ?2 AND role = 'assistant' AND active = 1 \
               AND id = (SELECT MAX(id) FROM messages \
                         WHERE session_id = ?2 AND role = 'assistant' AND active = 1)",
            rusqlite::params![unified_steps_json, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update unified_steps: {e}")))?;
        Ok(())
    }

    /// Save a human-readable session log as markdown.
    /// **Fixed filename**: `<session_id>.md` (overwrite, not new-file-per-save).
    /// Old timestamp-prefixed files (`{ts}-<sid>.md`) are moved to `orphans/`
    /// on first write; see `recover_orphan_session_logs`.
    /// Append-only JSONL mirror of the persisted timeline — Codex rollout
    /// 双轨 analog：`sessions.db` 损坏时可按行重建对话。每行一个 JSON
    /// 对象（role/content/tool_calls/reasoning）。
    ///
    /// ## High-water mark is a fingerprint, not a line count
    ///
    /// The old implementation read the whole file, counted lines and appended
    /// from that index. Two ways that breaks:
    /// - **Compression / `/undo` shrink the timeline.** `persistable.len() <=
    ///   already` then returned early, so the mirror stopped recording *new*
    ///   turns forever, silently — the one situation (long session, damaged
    ///   DB) where the mirror is supposed to matter.
    /// - **A torn tail** (killed mid-`writeln!`) was counted as a line, so the
    ///   next append started at the wrong offset.
    ///
    /// Now the anchor is the *identity of the last mirrored message*: read the
    /// last complete line (tail probe, not whole file), repair a torn tail,
    /// and resume right after the matching message. No match ⇒ rewrite the
    /// file. Every failure mode is self-healing, which also makes the rewrite
    /// deliberately non-atomic: the DB is the source of truth, and a partial
    /// rewrite is repaired on the next call.
    pub fn save_message_mirror(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), AgentError> {
        use std::io::Write as _;

        std::fs::create_dir_all(&self.sessions_dir)
            .map_err(|e| AgentError::Io(format!("Failed to create sessions dir: {e}")))?;
        let path = self
            .sessions_dir
            .join(format!("{}.jsonl", crate::session_persistence::session_stem(session_id)));

        let persistable: Vec<&Message> = messages
            .iter()
            .filter(|m| !m.ephemeral && !matches!(m.role, MessageRole::System))
            .collect();
        if persistable.is_empty() {
            return Ok(());
        }

        let start = read_last_complete_line(&path)
            .map_err(|e| AgentError::Io(format!("Failed to read JSONL mirror: {e}")))?
            .and_then(|line| line_fingerprint(&line))
            .and_then(|fp| {
                persistable
                    .iter()
                    .rposition(|m| message_fingerprint(m) == fp)
                    .map(|i| i + 1)
            });

        let render = |m: &Message| {
            serde_json::json!({
                "ts": Utc::now().to_rfc3339(),
                "role": match m.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                    MessageRole::Automation => "automation",
                },
                "content": m.content,
                "reasoning_content": m.reasoning_content,
                "tool_calls": m.tool_calls,
                "tool_call_id": m.tool_call_id,
                "name": m.name,
            })
            .to_string()
        };

        match start {
            // Anchor found and nothing new since: the common steady state.
            Some(start) if start == persistable.len() => Ok(()),
            Some(start) => {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| AgentError::Io(format!("Failed to open JSONL mirror: {e}")))?;
                for m in &persistable[start..] {
                    writeln!(f, "{}", render(m)).map_err(|e| {
                        AgentError::Io(format!("Failed to append JSONL mirror: {e}"))
                    })?;
                }
                // Survive a kill: without this the file keeps a torn tail that
                // the next run has to repair (and that a naive reader would
                // count as a message).
                f.sync_all()
                    .map_err(|e| AgentError::Io(format!("Failed to sync JSONL mirror: {e}")))?;
                Ok(())
            }
            // No anchor (missing file, shrunk timeline, torn-beyond-repair):
            // rebuild from the in-memory timeline.
            None => {
                let body: String = persistable
                    .iter()
                    .map(|m| format!("{}\n", render(m)))
                    .collect();
                std::fs::write(&path, body)
                    .map_err(|e| AgentError::Io(format!("Failed to rewrite JSONL mirror: {e}")))?;
                Ok(())
            }
        }
    }

    pub fn save_session_log(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.sessions_dir)
            .map_err(|e| AgentError::Io(format!("Failed to create sessions dir: {e}")))?;

        // Migrate old timestamp-prefixed files on first write
        Self::recover_orphan_session_logs(&self.sessions_dir, session_id);

        let filename = format!("{}.md", crate::session_persistence::session_stem(session_id));
        let path = self.sessions_dir.join(&filename);

        let mut content = String::new();
        content.push_str(&format!("# Session: {session_id}\n\n"));
        if let Some(m) = model {
            content.push_str(&format!("Model: {m}\n"));
        }
        content.push_str(&format!(
            "Date: {}\n\n---\n\n",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ));

        for msg in messages {
            // Skip system messages in exported session logs too —
            // markdown logs are intended for human review of a
            // conversation, and dumping the full reconstructed prompt
            // there is the same leak as dumping it into the chat UI.
            if matches!(msg.role, MessageRole::System) {
                continue;
            }
            let role_label = match msg.role {
                MessageRole::System => "🔧 System", // unreachable due to continue
                MessageRole::User => "👤 User",
                MessageRole::Assistant => "🤖 Assistant",
                MessageRole::Tool => "🔨 Tool",
                // Distinct emoji from User: the whole point of the role
                // is that nobody in the conversation said this.
                MessageRole::Automation => "⏰ 定时任务",
            };

            content.push_str(&format!("### {role_label}\n\n"));

            if let Some(ref text) = msg.content {
                //+: redact PII/secrets from the
                // human-readable log file. The log lives
                // on disk and can be tailed by humans, so
                // it gets the same treatment as the DB
                // row.
                content.push_str(&crate::pii_redact::redact_secrets(text));
                content.push_str("\n\n");
            }

            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    let safe_args =
                        crate::pii_redact::redact_secrets(&tc.function.arguments);
                    content.push_str(&format!(
                        "**Tool call:** `{}({})`\n\n",
                        tc.function.name, safe_args
                    ));
                }
            }
        }

        std::fs::write(&path, &content)
            .map_err(|e| AgentError::Io(format!("Failed to write session log: {e}")))?;

        Ok(path)
    }

    /// Save messages in trajectory format for RL training.
    /// **Fixed filename**: `<session_id>.json` (overwrite, not new-file-per-save).
    pub fn save_trajectory(
        &self,
        session_id: &str,
        messages: &[Message],
        user_query: &str,
        completed: bool,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.trajectories_dir)
            .map_err(|e| AgentError::Io(format!("Failed to create trajectories dir: {e}")))?;

        let filename = format!("{}.json", crate::session_persistence::session_stem(session_id));
        let path = self.trajectories_dir.join(&filename);

        let trajectory = serde_json::json!({
            "session_id": session_id,
            "user_query": user_query,
            "completed": completed,
            "timestamp": Utc::now().to_rfc3339(),
            "messages": messages,
            "turn_count": messages.iter().filter(|m| m.role == MessageRole::Assistant).count(),
        });

        let json_str = serde_json::to_string_pretty(&trajectory)
            .map_err(|e| AgentError::Io(format!("Failed to serialize trajectory: {e}")))?;

        std::fs::write(&path, &json_str)
            .map_err(|e| AgentError::Io(format!("Failed to write trajectory: {e}")))?;

        Ok(path)
    }

    /// Migrate old timestamp-prefixed session log files to the new
    /// fixed-name scheme. Scans `<sessions_dir>/` for files matching
    /// `*-<session_id>.md`, moves them to `<sessions_dir>/orphans/`.
    /// Called once per `save_session_log` call for the given session_id
    /// (idempotent — the renamed file won't match the pattern again).
    pub fn recover_orphan_session_logs(sessions_dir: &Path, session_id: &str) {
        let Ok(entries) = std::fs::read_dir(sessions_dir) else { return };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Match: anything like `2026-06-18_130000-<sid>.md`。消毒规则与
            // 写入端一致（见 `session_stem`），否则 `im:device:*` 这类 id
            // 永远匹配不上自己写的文件。
            let stem = crate::session_persistence::session_stem(session_id);
            if name.ends_with(&format!("-{stem}.md")) && name != format!("{stem}.md") {
                let orphans_dir = sessions_dir.join("orphans");
                let _ = std::fs::create_dir_all(&orphans_dir);
                let dest = orphans_dir.join(&name);
                let _ = std::fs::rename(&entry.path(), &dest);
            }
        }
    }

    /// Load persisted full system prompt for prefix-cache continuity (Python `sessions.system_prompt`).
    pub fn get_system_prompt(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare("SELECT system_prompt FROM sessions WHERE id = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;
        match stmt.query_row(rusqlite::params![session_id], |r| {
            r.get::<_, Option<String>>(0)
        }) {
            Ok(s) => Ok(s.filter(|t| !t.trim().is_empty())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("Failed to read system_prompt: {e}"))),
        }
    }

    /// read back the persisted workspace / expert_id
    /// for the given session. Returns `(Option<String>, Option<String>)`
    /// in `(workspace, expert_id)` order. Used by the
    /// `test_persist_session_writes_workspace_and_survives_coalesce`
    /// regression test and by the server's reload path if a future
    /// caller wants a single-shot lookup instead of going through
    /// `list_sessions`.
    pub fn read_session_metadata(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare("SELECT workspace, expert_id FROM sessions WHERE id = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare metadata query: {e}")))?;
        match stmt.query_row(rusqlite::params![session_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        }) {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((None, None)),
            Err(e) => Err(AgentError::Io(format!("Failed to read session metadata: {e}"))),
        }
    }

    /// Load a previous session from SQLite.
    /// **Filters out approval rows** (`role='approval'`) — those live in
    /// the same `messages` table for ordering convenience, but they
    /// carry no LLM-relevant content and would otherwise pollute the
    /// conversation history with bogus user messages. Use
    /// [`Self::load_session_with_approvals`] for the UI-facing timeline.
    pub fn load_session(&self, session_id: &str) -> Result<Vec<Message>, AgentError> {
        // Defensive: most call sites go through `persist_session` /
        // `send_message` which already ensure_db, but `rename_session`
        // hits this directly and would otherwise crash on a pre-migration
        // DB (no `active` column yet → "no such column: active").
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        let mut stmt = conn
            .prepare(
                // role != 'approval' keeps approval rows out of the LLM
                // payload; role != 'system' keeps the reconstructed
                // prompt (AGENTS.md + skills, ~23k chars) out of the
                // returned timeline — it's rebuilt per-turn by
                // `agent_loop`, never re-shown to the user.
                // role != 'automation' is the same exclusion for a
                // different reason: an automation result IS worth
                // showing, but nobody in the conversation said it, so
                // letting the model read it back as context would have
                // it act on a turn that never happened.
                "SELECT role, content, tool_call_id, tool_calls, name, execution_timeline, reasoning_content, unified_steps
                 FROM messages
                 WHERE session_id = ?1 AND active = 1
                   AND role != 'approval' AND role != 'system'
                   AND role != 'automation'
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;

        let messages = stmt
            .query_map(rusqlite::params![session_id], |row| {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let tool_call_id: Option<String> = row.get(2)?;
                let tool_calls_json: Option<String> = row.get(3)?;
                let db_name: Option<String> = row.get(4)?;
                let timeline_json: Option<String> = row.get(5)?;
                let reasoning_content: Option<String> = row.get(6)?;
                let unified_steps_json: Option<String> = row.get(7)?;

                let role = match role_str.as_str() {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                };

                let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());
                let execution_timeline = timeline_json.and_then(|json| serde_json::from_str(&json).ok());
                let unified_steps = unified_steps_json.and_then(|json| serde_json::from_str(&json).ok());

                Ok(Message {
                    role,
                    content,
                    content_parts: None,
                    tool_calls,
                    tool_call_id,
                    name: db_name,
                    reasoning_content,
                    cache_control: None,
                    execution_timeline,
                    unified_steps,
                    ephemeral: false,
                    // Nothing to restore: this is the LLM-side read and
                    // automation rows never reach it (filtered by the
                    // WHERE clause above). `load_session_with_approvals`
                    // is the read that carries provenance.
                    automation_meta: None,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        Ok(messages)
    }

    /// Load the compressed context summary for a session.
    /// Returns `Some(summary)` if a previous compression produced a summary
    /// that was persisted, `None` otherwise. The caller (agent loop) should
    /// inject this as a System message at the start of the conversation
    /// to maintain cross-session context continuity.
    pub fn load_compressed_summary(&self, session_id: &str) -> Option<String> {
        let conn = open_with_wal(&self.db_path).ok()?;
        let mut stmt = conn
            .prepare("SELECT compressed_summary FROM sessions WHERE id = ?1")
            .ok()?;
        let result = stmt
            .query_row(rusqlite::params![session_id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()?;
        result.filter(|s| !s.is_empty())
    }

    /// (A): archive the *raw* turns that the compressor is
    /// about to fold into a handoff summary. Append-only — we never
    /// delete from `compression_archives` after a write, so the
    /// original wording survives even after `flush_messages` has
    /// wiped it from `messages` and the LLM has moved on.
    /// `archived_turns` is JSON-encoded as a single TEXT column so the
    /// schema doesn't have to mirror the `Message` Rust struct (which
    /// keeps evolving — `content_parts`, `cache_control`, etc.). The
    /// trade-off is no FTS5 over the archive, which is fine because
    /// the recall path uses `MemoryRouter::recognize` instead.
    pub fn archive_compressed_turns(
        &self,
        session_id: &str,
        archived_turns: &[Message],
        original_turns: usize,
        compressed_turns: usize,
        original_tokens: u64,
        compressed_tokens: u64,
    ) -> Result<(), AgentError> {
        if archived_turns.is_empty() {
            return Ok(());
        }
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        let content_json = serde_json::to_string(archived_turns).map_err(|e| {
            AgentError::Io(format!("Failed to serialise archived turns: {e}"))
        })?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO compression_archives
                (session_id, content, original_turns, compressed_turns,
                 original_tokens, compressed_tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                session_id,
                content_json,
                original_turns as i64,
                compressed_turns as i64,
                original_tokens as i64,
                compressed_tokens as i64,
                now,
            ],
        )
        .map_err(|e| AgentError::Io(format!("Failed to insert compression archive: {e}")))?;
        Ok(())
    }

    /// (B): append a row to the append-only summary
    /// history. Pairs every compression event with its metrics so a
    /// session's full compaction arc is queryable. The
    /// `sessions.compressed_summary` column is still written by
    /// `save_session_atomic` as the fast-load "latest summary"
    /// index; this table is the historical trail.
    pub fn append_compression_summary_history(
        &self,
        session_id: &str,
        summary: &str,
        compression_count: u32,
        original_turns: usize,
        compressed_turns: usize,
        original_tokens: u64,
        compressed_tokens: u64,
    ) -> Result<(), AgentError> {
        if summary.is_empty() {
            return Ok(());
        }
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let now = Utc::now().to_rfc3339();
        let tokens_saved = original_tokens.saturating_sub(compressed_tokens) as i64;
        conn.execute(
            "INSERT INTO compression_summary_history
                (session_id, summary, compression_count, original_turns,
                 compressed_turns, original_tokens, compressed_tokens,
                 tokens_saved, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                session_id,
                summary,
                compression_count as i64,
                original_turns as i64,
                compressed_turns as i64,
                original_tokens as i64,
                compressed_tokens as i64,
                tokens_saved,
                now,
            ],
        )
        .map_err(|e| AgentError::Io(format!(
            "Failed to insert compression summary history: {e}"
        )))?;
        Ok(())
    }

    /// (A): load the raw archive rows for a session, newest
    /// first. Used by the cross-session recall path to recover
    /// original wording that the compressor dropped from the in-memory
    /// conversation.
    pub fn load_compression_archive(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<CompressionArchiveRow>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT id, content, original_turns, compressed_turns,
                        original_tokens, compressed_tokens, salience,
                        source, created_at
                 FROM compression_archives
                 WHERE session_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare archive query: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, limit as i64], |row| {
                Ok(CompressionArchiveRow {
                    id: row.get::<_, i64>(0)?,
                    content: row.get::<_, String>(1)?,
                    original_turns: row.get::<_, i64>(2)? as usize,
                    compressed_turns: row.get::<_, i64>(3)? as usize,
                    original_tokens: row.get::<_, i64>(4)? as u64,
                    compressed_tokens: row.get::<_, i64>(5)? as u64,
                    salience: row.get::<_, f64>(6)?,
                    source: row.get::<_, String>(7)?,
                    created_at: row.get::<_, String>(8)?,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query archive: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AgentError::Io(format!("archive row decode: {e}")))?);
        }
        Ok(out)
    }

    /// (B): load the append-only summary history for a
    /// session, oldest first (chronological reading order). The caller
    /// can concatenate the summaries to reconstruct the full
    /// compaction trail.
    pub fn load_compression_summary_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<CompressionHistoryRow>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT id, summary, compression_count, original_turns,
                        compressed_turns, original_tokens, compressed_tokens,
                        tokens_saved, created_at
                 FROM compression_summary_history
                 WHERE session_id = ?1
                 ORDER BY id ASC
                 LIMIT ?2",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare history query: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, limit as i64], |row| {
                Ok(CompressionHistoryRow {
                    id: row.get::<_, i64>(0)?,
                    summary: row.get::<_, String>(1)?,
                    compression_count: row.get::<_, i64>(2)? as u32,
                    original_turns: row.get::<_, i64>(3)? as usize,
                    compressed_turns: row.get::<_, i64>(4)? as usize,
                    original_tokens: row.get::<_, i64>(5)? as u64,
                    compressed_tokens: row.get::<_, i64>(6)? as u64,
                    tokens_saved: row.get::<_, i64>(7)? as u64,
                    created_at: row.get::<_, String>(8)?,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query history: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AgentError::Io(format!("history row decode: {e}")))?);
        }
        Ok(out)
    }

    /// Append a new approval row to the `messages` table. Called from the
    /// server the moment a `RequestApproval` event is emitted so the
    /// request is durable even if the stream is interrupted before the
    /// user responds (or the user reloads the page). If a row with the
    /// same `request_id` already exists, this is a no-op (idempotent —
    /// protects against retried emits).
    pub fn append_approval_message(
        &self,
        session_id: &str,
        record: &ApprovalRecord,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // Idempotency check: another agent_loop may have already written
        // the same request_id. Skip if so.
        let already = {
            let mut stmt = conn
                .prepare(
                    "SELECT 1 FROM messages
                     WHERE session_id = ?1 AND role = 'approval' AND extra LIKE ?2
                     LIMIT 1",
                )
                .map_err(|e| AgentError::Io(format!("Failed to prepare idem check: {e}")))?;
            let needle = format!("%\"request_id\":\"{}\"%", record.request_id);
            match stmt.query_row(rusqlite::params![session_id, needle], |_| Ok(())) {
                Ok(_) => true,
                Err(rusqlite::Error::QueryReturnedNoRows) => false,
                Err(e) => {
                    return Err(AgentError::Io(format!("Failed idem check: {e}")));
                }
            }
        };
        if already {
            return Ok(());
        }

        //+: scrub PII/secrets from the approval
        // row's `arguments` field before persisting. The raw
        // tool args can contain bearer tokens / API keys
        // (e.g. `terminal` tool with a curl command).
        let mut safe_args = record.arguments.clone();
        if let serde_json::Value::Object(ref mut map) = safe_args {
            // Walk every string leaf.
            for (_, v) in map.iter_mut() {
                if let serde_json::Value::String(s) = v {
                    *s = crate::pii_redact::redact_secrets(s);
                }
            }
        } else if let serde_json::Value::String(ref mut s) = safe_args {
            *s = crate::pii_redact::redact_secrets(s);
        }
        let mut safe_record = record.clone();
        safe_record.arguments = safe_args;

        let extra = serde_json::to_string(&safe_record)
            .map_err(|e| AgentError::Io(format!("Failed to serialize approval record: {e}")))?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, created_at, active, extra)
             VALUES (?1, 'approval', NULL, NULL, NULL, ?2, 1, ?3)",
            rusqlite::params![session_id, now, extra],
        )
        .map_err(|e| AgentError::Io(format!("Failed to insert approval row: {e}")))?;

        Ok(())
    }

    /// Update the decision on a previously-persisted approval row.
    /// Called from the server the moment the user clicks allow/deny.
    /// Looks up the row by `request_id` (matched inside the `extra`
    /// JSON blob — there's no separate column for it), so it's robust
    /// against duplicate emits.
    pub fn resolve_approval_message(
        &self,
        session_id: &str,
        request_id: &str,
        decision: &str,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // Read the existing record, update, write back. We don't try to
        // do a partial JSON patch in SQL — the row is small and a
        // round-trip keeps the logic obvious.
        let needle = format!("%\"request_id\":\"{}\"%", request_id);
        let existing_extra: Option<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT extra FROM messages
                     WHERE session_id = ?1 AND role = 'approval' AND extra LIKE ?2
                     LIMIT 1",
                )
                .map_err(|e| AgentError::Io(format!("Failed to prepare resolve lookup: {e}")))?;
            match stmt.query_row(rusqlite::params![session_id, needle], |row| {
                row.get::<_, Option<String>>(0)
            }) {
                Ok(s) => s,
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    return Err(AgentError::Io(format!(
                        "Failed to lookup approval row: {e}"
                    )));
                }
            }
        };
        let Some(raw) = existing_extra else {
            // No matching row — nothing to update. This can happen if the
            // agent emitted the request but the user responded before we
            // managed to persist it (race). Silent no-op is fine; the
            // in-memory `extra` JSON is still the source of truth for
            // the running session.
            return Ok(());
        };
        let mut record: ApprovalRecord = serde_json::from_str(&raw).unwrap_or_else(|_| {
            // Corrupt row — overwrite with a synthetic record so the
            // decision is at least captured for the UI.
            ApprovalRecord {
                request_id: request_id.to_string(),
                tool: "<unknown>".to_string(),
                arguments: serde_json::Value::Null,
                risk: "caution".to_string(),
                decision: None,
                decided_at: None,
                expires_at: None,
                linked_assistant_id: None,
                diff: None,
            }
        });
        record.decision = Some(decision.to_string());
        record.decided_at = Some(Utc::now().to_rfc3339());
        let new_extra = serde_json::to_string(&record)
            .map_err(|e| AgentError::Io(format!("Failed to serialize updated approval: {e}")))?;
        // Match on the raw substring to identify the row to update —
        // the request_id is embedded inside the JSON so we can't
        // address it by column. We additionally constrain by session_id
        // and role to be safe.
        conn.execute(
            "UPDATE messages SET extra = ?1
             WHERE session_id = ?2 AND role = 'approval' AND extra LIKE ?3",
            rusqlite::params![new_extra, session_id, needle],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update approval row: {e}")))?;

        Ok(())
    }

    /// Sweep all approval rows still in the pending state and stamp them
    /// with `decision = "superseded"`. Called once at server startup so a
    /// crashed/killed server never leaves the next session staring at a
    /// half-answered card — every abandoned request is auto-resolved and
    /// the front-end renders the "已超时 · 操作已自动取消" chip instead of
    /// a clickable button. Already-resolved rows (`decision != null`)
    /// are left untouched; only pending ones (`decision IS NULL`) flip.
    /// Returns the number of rows actually superseded so the caller can
    /// log the count for diagnostics.
    ///
    /// Implementation note: we lean on SQLite's `json_set` / `json_extract`
    /// (available via `rusqlite`'s `modern_sqlite` feature) instead of
    /// round-tripping through Rust for every row — startup touches every
    /// pending card in the whole DB at once, not one at a time.
    pub fn supersede_pending_approvals(&self) -> Result<usize, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let now = Utc::now().to_rfc3339();
        // `json_valid(extra)` guards against legacy/corrupt blobs (the
        // `extra` column was added before strict JSON validation). If the
        // blob isn't valid JSON we skip it — better to leave the row
        // untouched than to overwrite it with garbage via `json_set`.
        // `json_extract(extra, '$.decision') IS NULL` catches both
        // `decision: null` (current schema) and rows missing the field
        // entirely (very old data).
        let n = conn
            .execute(
                "UPDATE messages
                 SET extra = json_set(extra,
                                      '$.decision', 'superseded',
                                      '$.decided_at', ?1)
                 WHERE role = 'approval'
                   AND json_valid(extra)
                   AND json_extract(extra, '$.decision') IS NULL",
                rusqlite::params![now],
            )
            .map_err(|e| AgentError::Io(format!(
                "Failed to supersede pending approvals at startup: {e}"
            )))?;
        Ok(n)
    }

    /// Load the full conversation timeline (LLM messages + approval rows)
    /// in arrival order. This is what the UI wants for `get_session_messages`.
    /// Approval rows are returned as `SessionEntry::Approval` so the
    /// caller can render them as inline cards in the right slot.
    /// IMPORTANT: approval rows are NOT included in the returned
    /// `Message`s — they live only in the timeline. If you need them as
    /// LLM context, filter the `Message` variants out of the result
    /// yourself (this is what `load_session` already does).
    /// 一列 `messages` 行 → `SessionEntry`。列序见 `SESSION_ENTRY_COLUMNS`。
    ///
    /// 抽成关联函数是为了让「全量读」与「尾部窗口读」共用同一套行解析 ——
    /// 两处各写一遍时，任何一列的增减都会让其中一条悄悄读错位。
    fn session_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionEntry> {
        let role_str: String = row.get(0)?;
        let content: Option<String> = row.get(1)?;
        let tool_call_id: Option<String> = row.get(2)?;
        let tool_calls_json: Option<String> = row.get(3)?;
        let extra_json: Option<String> = row.get(4)?;
        let db_name: Option<String> = row.get(5)?;
        let timeline_json: Option<String> = row.get(6)?;
        let reasoning_content: Option<String> = row.get(7)?;
        let unified_steps_json: Option<String> = row.get(8)?;

        let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());

        if role_str == "approval" {
            let record: ApprovalRecord = extra_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
            return Ok(if record.linked_assistant_id.is_some() {
                SessionEntry::Approval(record)
            } else {
                SessionEntry::ApprovalOrphan(record)
            });
        }

        let role = match role_str.as_str() {
            "system" => MessageRole::System,
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            "automation" => MessageRole::Automation,
            // Unknown role — skip rather than crash. Future
            // schema additions shouldn't take down old clients.
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        let execution_timeline = timeline_json.and_then(|json| serde_json::from_str(&json).ok());
        let unified_steps = unified_steps_json.and_then(|json| serde_json::from_str(&json).ok());
        // Automation provenance. A missing or corrupt blob yields `None`,
        // and the UI then renders an unnamed task header — which is still
        // honest, whereas failing the row would drop the result entirely.
        let automation_meta = extra_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<AutomationMeta>(s).ok());
        Ok(SessionEntry::Message(Message {
            role,
            content,
            content_parts: None,
            tool_calls,
            tool_call_id,
            name: db_name,
            reasoning_content,
            cache_control: None,
            execution_timeline,
            unified_steps,
            ephemeral: false,
            automation_meta,
        }))
    }

    pub fn load_session_with_approvals(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionEntry>, AgentError> {
        // Defensive — see `load_session` for rationale.
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        let mut stmt = conn
            .prepare(
                // role != 'system' is the defensive counterpart to the
                // write-side filter in `flush_messages_to_session_db`.
                // Pre-existing rows from before that filter are
                // excluded here so a reloaded session never paints the
                // 23k-char system prompt into the chat timeline.
                "SELECT role, content, tool_call_id, tool_calls, extra, name, execution_timeline, reasoning_content, unified_steps
                 FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'system'
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare timeline query: {e}")))?;

        let entries = stmt
            .query_map(
                rusqlite::params![session_id],
                Self::session_entry_from_row,
            )
            .map_err(|e| AgentError::Io(format!("Failed to query timeline: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read timeline: {e}")))?;

        Ok(entries)
    }

    /// same as `load_session_with_approvals` but
    /// also returns the persisted `created_at` timestamp alongside
    /// each entry. Used by the HTTP `GET /v1/sessions/{id}/messages`
    /// handler to populate `ProtocolMessage.created_at` so the
    /// desktop client shows the real relative time ("3 天前") on
    /// session reload instead of falling back to `now` at serve
    /// time (which produced "刚刚 / 1 分钟前" for every reloaded
    /// message).
    /// Implementation: a second SELECT against the same `messages`
    /// table, sharing the same WHERE / ORDER BY clause as
    /// `load_session_with_approvals`. Both result sets are returned
    /// in the same row order — we zip them positionally. No
    /// second source-of-truth field on `Message` is added; this
    /// stays scoped to the wire DTO path.
    /// 尾部窗口版：只读最后 `limit` 条（过滤条件与全量版一致），返回仍按
    /// `id` 升序 —— 调用方拿到的顺序不变，只是前面少了被截掉的旧消息。
    ///
    /// 为什么需要它：前端首屏只要尾部 60 条（`INITIAL_MESSAGE_WINDOW`），
    /// 而全量版会把整段历史读进内存再丢掉 97%。长会话的打开延迟与峰值内存
    /// 都由这条 SQL 决定。
    pub fn load_session_tail_with_timestamps(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<(SessionEntry, Option<String>)>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_call_id, tool_calls, extra, name, \
                        execution_timeline, reasoning_content, unified_steps, created_at
                 FROM (SELECT * FROM messages
                       WHERE session_id = ?1 AND active = 1 AND role != 'system'
                       ORDER BY id DESC LIMIT ?2)
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare tail query: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, limit as i64], |row| {
                Ok((
                    Self::session_entry_from_row(row)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query tail: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read tail: {e}")))?;
        Ok(rows)
    }

    pub fn load_session_with_approvals_and_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Vec<(SessionEntry, Option<String>)>, AgentError> {
        let entries = self.load_session_with_approvals(session_id)?;

        // Re-query for `created_at` with the *exact* same SQL
        // filter as `load_session_with_approvals` so the two row
        // streams align 1:1 by position. Any drift would manifest
        // as a misaligned timestamp, which the UI would render as
        // "刚刚" — same bug we're trying to fix. The filter is
        // intentionally duplicated rather than refactored into a
        // shared constant because the SQL strings are also the
        // SQL-level documentation of what counts as "a row".
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT created_at FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'system'
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare timestamp query: {e}")))?;
        let ts_vec: Vec<String> = stmt
            .query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))
            .map_err(|e| AgentError::Io(format!("Failed to query timestamps: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to collect timestamps: {e}")))?;

        let out: Vec<(SessionEntry, Option<String>)> = entries
            .into_iter()
            .zip(ts_vec.into_iter().map(Some).chain(std::iter::repeat(None)))
            .map(|(e, ts)| (e, ts))
            .collect();
        Ok(out)
    }

    /// Return all direct children of `session_id` ordered by creation
    /// time ascending (oldest first). A session is a child when its
    /// `parent_session_id` column matches the given id. Returns an
    /// empty vec when there are no children or when the parent itself
    /// doesn't exist.
    pub fn find_children(&self, session_id: &str) -> Result<Vec<String>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM sessions
                 WHERE parent_session_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare find_children query: {e}")))?;

        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![session_id], |row| row.get(0))
            .map_err(|e| AgentError::Io(format!("Failed to query children: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read children: {e}")))?;

        Ok(ids)
    }

    /// Return the parent session ID of the given session, if any.
    /// This enables climbing the lineage chain upward from a child
    /// to its root ancestor.
    pub fn get_parent(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare("SELECT parent_session_id FROM sessions WHERE id = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare get_parent query: {e}")))?;

        let result: Option<String> = stmt
            .query_row(rusqlite::params![session_id], |row| row.get(0))
            .ok()
            .flatten();
        Ok(result)
    }

    /// (, Hermes 对标): create a real child session
    /// that materialises the compressed summary as its own row,
    /// linked to the parent via `parent_session_id`.
    /// Returns the newly-generated `child_session_id`, which is
    /// deterministic (`{parent}-c{N}` where N is the parent's
    /// existing child count + 1) so repeated calls with the same
    /// `(parent_id, summary)` produce the same id and the
    /// `INSERT OR IGNORE` style logic in the SQL makes the
    /// operation idempotent.
    /// The child's `messages` table contains a single system-role
    /// message carrying the summary verbatim — that's what the
    /// frontend lineage browser displays when the user clicks
    /// the child node ("这里是你压缩前的内容").
    //
    //Bug #3 — finalize 路径同步 I/O 阻塞 WS heartbeat
    //此函数内 N×autocommit INSERT 无事务包装，3-6s 阻塞 > 15s heartbeat
    //   → 前端 was_client_disconnect=true close。
    //修复规则（ 代码化）：finalize 后置副作用必须 spawn_blocking
    //   fire-and-forget，禁止在 async 路径中 await。
    //   测试：session_persistence_tests::test_compression_child_session_idempotent
    pub fn create_compression_child_session(
        &self,
        parent_session_id: &str,
        compression: &CompressionSnapshot,
        model: Option<&str>,
        platform: Option<&str>,
    ) -> Result<String, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // Idempotent child id: count existing children and append
        // `-c{count+1}`. Stable across process restarts because
        // the count is derived from the DB, not a counter.
        let existing_children: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE parent_session_id = ?1",
                rusqlite::params![parent_session_id],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count children: {e}")))?;
        let child_id = format!("{parent_session_id}-c{}", existing_children + 1);

        let now = Utc::now().to_rfc3339();
        let title = format!(
            "[Compressed #{} from {}]",
            compression.compression_count.max(1),
            // Truncate parent id so the title fits; show the
            // last 12 chars (after stripping any platform
            // prefix) for human-recognition.
            parent_session_id
                .rsplit(':')
                .next()
                .unwrap_or(parent_session_id)
                .chars()
                .rev()
                .take(12)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );

        conn.execute(
            "INSERT OR IGNORE INTO sessions (
                id, model, platform, created_at, updated_at,
                title, message_count, system_prompt,
                compressed_summary, parent_session_id
             )
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, 1, ?6, ?7, ?8)",
            rusqlite::params![
                child_id,
                model,
                platform.unwrap_or("local"),
                now,
                title,
                // system_prompt carries the summary text so the
                // lineage browser can show the full payload
                // without re-querying `compressed_summary`.
                compression.summary.as_str(),
                compression.summary.as_str(),
                parent_session_id,
            ],
        )
        .map_err(|e| AgentError::Io(format!("Failed to insert compression child: {e}")))?;

        //NOTE : we deliberately do NOT insert the summary
        // as a `role='system'` message row. The schema migration
        // in `ensure_db` runs `DELETE FROM messages WHERE role =
        // 'system'` and would silently purge our summary on the
        // next ensure_db call. The summary is recoverable via
        // `load_compressed_summary(&child_id)` (it lives on the
        // `sessions.compressed_summary` column), which the
        // frontend lineage browser uses to render the "what got
        // folded" view.

        // Mirror the raw archived turns so a deep-link to the
        // child can still recover the pre-compression wording
        // (compression_archives is keyed by session_id, so the
        // child gets its own copy without disturbing the
        // parent's).
        for (i, turn) in compression.archived_turns.iter().enumerate() {
            let content = turn
                .content
                .as_ref()
                .map(|c| c.as_str())
                .unwrap_or_default();
            let role = match turn.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                // Excluded from the LLM payload everywhere else, so it
                // must not sneak back in through the archive's child
                // session either — that child is a browsable summary of
                // what the model actually saw.
                MessageRole::Automation => "automation",
            };
            conn.execute(
                "INSERT INTO messages (session_id, role, content, created_at, active)
                 VALUES (?1, ?2, ?3, ?4, 1)",
                rusqlite::params![child_id, role, content, now],
            )
            .map_err(|e| {
                AgentError::Io(format!("Failed to insert child archived turn #{i}: {e}"))
            })?;
        }

        tracing::info!(
            target: "hermix.lineage",
            parent = %parent_session_id,
            child = %child_id,
            compression_count = compression.compression_count,
            archived_turns = compression.archived_turns.len(),
            "compression lineage: created child session"
        );

        Ok(child_id)
    }

    /// Return basic session metadata (title and parent_id) for lineage display.
    pub fn get_session_title(&self, session_id: &str) -> Result<Option<(String, Option<String>)>, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn
            .prepare("SELECT title, parent_session_id FROM sessions WHERE id = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare get_session_title query: {e}")))?;

        let result: Option<(String, Option<String>)> = stmt
            .query_row(rusqlite::params![session_id], |row| {
                Ok((row.get::<_, Option<String>>(0)?.unwrap_or_default(), row.get::<_, Option<String>>(1)?))
            })
            .ok();
        Ok(result)
    }

    /// Soft-delete the most recent `count` user-to-assistant turns of a
    /// session. A turn is a contiguous slice of messages that starts
    /// with a user message and ends just before the next user message
    /// (or at the end of the conversation). The corresponding tool
    /// messages are also marked inactive.
    /// Returns the number of messages that were deactivated.
    pub fn mark_recent_turns_inactive(
        &self,
        session_id: &str,
        count: usize,
    ) -> Result<usize, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // Walk the messages backwards, group by turns.
        let mut stmt = conn
            .prepare(
                // `role != 'automation'`: an automation result is not a
                // conversational turn, so it must neither be counted as
                // one ("undo 1 turn" would swallow the user's last real
                // message and leave a scheduler result behind) nor be
                // deleted by the undo — the user never asked for it to
                // go away.
                "SELECT id, role FROM messages
                 WHERE session_id = ?1 AND active = 1
                   AND role != 'approval' AND role != 'automation'
                 ORDER BY id DESC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;
        let rows: Vec<(i64, String)> = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        let mut turns: Vec<Vec<i64>> = Vec::new();
        let mut current: Vec<i64> = Vec::new();
        // Walking backwards: a "user" message extends the current turn
        // (it must be the start of the same turn — the assistant of
        // this turn is already in `current`). An "assistant" message
        // means we've crossed a turn boundary into the *previous*
        // turn, so close the current one and start a new buffer.
        for (id, role) in rows {
            if role == "assistant" && !current.is_empty() {
                current.reverse();
                turns.push(std::mem::take(&mut current));
            }
            current.push(id);
        }
        if !current.is_empty() {
            current.reverse();
            turns.push(current);
        }
        // `turns` is now in reverse-chronological order (latest first).
        // Take the most recent `count` turns from the front; flatten
        // preserves each turn's chronological order.
        let to_deactivate: Vec<i64> = turns
            .into_iter()
            .take(count)
            .flatten()
            .collect();
        if to_deactivate.is_empty() {
            return Ok(0);
        }

        // Also deactivate any approval rows that sit inside the id-range
        // covered by the turns being undone. We can't match by
        // `linked_assistant_id` directly (it's the frontend-generated
        // message id, not the DB row id), so we use a range heuristic:
        // any approval whose DB id falls between the smallest and
        // largest id in `to_deactivate` is considered "inside" a turn
        // being rolled back. This is a small over-approximation when a
        // single turn spans many DB rows (a multi-step tool call with
        // several tool messages interleaved), but it's correct for the
        // typical case — and in the worst case it just deactivates one
        // extra approval row that the user can still re-decide later.
        let mut extra_approval_ids: Vec<i64> = Vec::new();
        if !to_deactivate.is_empty() {
            let min_id = *to_deactivate.iter().min().unwrap();
            let max_id = *to_deactivate.iter().max().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM messages
                     WHERE session_id = ?1 AND active = 1 AND role = 'approval'
                     AND id >= ?2 AND id <= ?3",
                )
                .map_err(|e| AgentError::Io(format!("Failed to query approvals: {e}")))?;
            let rows: Vec<i64> = stmt
                .query_map(rusqlite::params![session_id, min_id, max_id], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|e| AgentError::Io(format!("Failed to read approvals: {e}")))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| AgentError::Io(format!("Failed to collect approvals: {e}")))?;
            extra_approval_ids = rows;
        }
        let mut to_deactivate_extended = to_deactivate.clone();
        to_deactivate_extended.extend(extra_approval_ids);
        let placeholders = to_deactivate_extended
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE messages SET active = 0 WHERE id IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(to_deactivate_extended.len());
        for id in &to_deactivate_extended {
            params.push(id);
        }
        let n = conn
            .execute(&sql, params.as_slice())
            .map_err(|e| AgentError::Io(format!("Failed to deactivate messages: {e}")))?;

        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to bump session timestamp: {e}")))?;

        Ok(n)
    }

    /// Restore the most recently deactivated block of messages. The
    /// block is the contiguous range of active=0 rows that ends at the
    /// highest id currently inactive. Returns the number of messages
    /// restored.
    pub fn restore_last_undone(
        &self,
        session_id: &str,
    ) -> Result<usize, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        let mut stmt = conn
            .prepare(
                "SELECT id, active FROM messages
                 WHERE session_id = ?1
                 ORDER BY id DESC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;
        let rows: Vec<(i64, bool)> = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        let mut to_restore: Vec<i64> = Vec::new();
        let mut seen_active = false;
        for (id, is_active) in rows {
            if is_active {
                seen_active = true;
                break;
            }
            to_restore.push(id);
        }
        if !seen_active || to_restore.is_empty() {
            return Ok(0);
        }

        let placeholders = to_restore
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE messages SET active = 1 WHERE id IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(to_restore.len());
        for id in &to_restore {
            params.push(id);
        }
        let n = conn
            .execute(&sql, params.as_slice())
            .map_err(|e| AgentError::Io(format!("Failed to restore messages: {e}")))?;

        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to bump session timestamp: {e}")))?;

        Ok(n)
    }

    /// P5 L3 seq 域（设计 §3.5）：查指定 session 的持久 seq 水位。
    /// 订阅端点用此值做增量补发分界——客户端携 `since_seq` 重连，
    /// 服务端返回 `seq > since_seq` 的已提交条目。`session_seq` 表
    /// 是独立单调计数器，不受 `flush_messages` 的 DELETE+INSERT 影响
    /// （后者会重置 `messages.id`，不能当 seq 用）。
    pub fn last_seq_for(&self, session_id: &str) -> Result<u64, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let last: Option<i64> = conn
            .query_row(
                "SELECT last_seq FROM session_seq WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(last.unwrap_or(0) as u64)
    }

    /// P5 L3 seq 域：为指定 session 分配下一个持久 seq 号。
    /// 原子 `INSERT … ON CONFLICT DO UPDATE`——每 session 一行
    /// `last_seq`，冲突时 `last_seq = last_seq + 1`，返回新值。
    /// 调用点：`save_session_atomic` 在每回合 flush 前取一次
    /// （seq 是**回合级**计数器，非消息级——设计 §3.5 的对账锚
    /// 粒度是"已提交的条目块"，一个回合的一次 flush 即一个锚）。
    pub fn next_seq_for(&self, session_id: &str) -> Result<u64, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let next: i64 = conn
            .query_row(
                r#"
                INSERT INTO session_seq (session_id, last_seq)
                VALUES (?1, 1)
                ON CONFLICT(session_id) DO UPDATE SET last_seq = last_seq + 1
                RETURNING last_seq
                "#,
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to allocate session seq: {e}")))?;
        Ok(next as u64)
    }
}
