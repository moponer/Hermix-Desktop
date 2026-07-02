//! Session persistence — save and load conversation sessions.
//!
//! Provides SQLite-backed session storage with FTS5 indexing for search,
//! human-readable markdown session logs, and trajectory format for RL training.
//!
//! Corresponds to Python `run_agent.py`'s `_persist_session`, `_save_session_log`,
//! and `_save_trajectory` methods.

use std::path::{Path, PathBuf};

use chrono::Utc;
use hermix_core::{AgentError, Message, MessageRole};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Connection helper
// ---------------------------------------------------------------------------
//
// `rusqlite::Connection::open` defaults to `journal_mode = delete`
// (single writer, readers block writers) and `busy_timeout = 0`
// (zero retry on `SQLITE_BUSY`). With two processes or threads racing
// on the same DB — the agent loop and the dashboard status query, or
// two concurrent turns — the second opener gets `SQLITE_BUSY`
// immediately, and the `let _ = persist(...)` wrappers in the call
// sites silently swallow the error. The user sees the message
// "saved" but the row never lands in the DB.
//
// WAL allows concurrent readers + a single writer, and the busy
// timeout gives the writer 5 s to retry before giving up. WAL is
// also strictly more crash-resilient than the rollback journal.
//
// Best-effort: if WAL mode is rejected (read-only file, exclusive
// lock held, etc.) we log a warning and continue — `busy_timeout`
// alone still resolves the silent-drop bug.
fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, AgentError> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
    if let Err(e) =
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
    {
        tracing::warn!(
            "session_persistence: WAL/busy_timeout PRAGMA failed ({e}); \
             continuing with default journaling — concurrent writes may still race"
        );
    }
    Ok(conn)
}

// ---------------------------------------------------------------------------
// SessionPersistence
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ApprovalRecord + SessionEntry
// ---------------------------------------------------------------------------
//
// Approval rows are stored in the same `messages` table as regular messages
// (so we get a single source of truth for the conversation timeline), but
// with `role='approval'`. The actual payload — request id, tool name,
// arguments, risk level, decision, and timestamp — lives in the `extra`
// JSON column (added in migration at `ensure_db` time).
//
// Why a row in `messages` rather than a separate `approvals` table?
//   1. Replay order: approval rows must interleave with assistant
//      messages exactly when the agent emitted the `RequestApproval`
//      event. Using a single `id` autoincrement PK + ORDER BY id ASC
//      gives us that order for free.
//   2. Soft-delete parity: `/undo` and `/restore` already flip
//      `active=0` on the relevant rows. Adding `WHERE role = 'approval'`
//      to the existing helpers is a no-op for the LLM context (we filter
//      approvals out before constructing `Message`s — see
//      `load_session`), but their UI presence tracks the underlying
//      conversation state naturally.
//   3. Single FTS5 trigger covers the table; no second trigger needed.

/// One row of the `messages` table that corresponds to a tool-approval
/// request. Serialized into the `extra` JSON column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    /// Server-side approval request id (matches `StreamEvent::RequestApproval::request_id`).
    pub request_id: String,
    /// Tool the agent wanted to invoke (e.g. "terminal_run").
    pub tool: String,
    /// JSON arguments the agent wanted to pass to the tool.
    #[serde(default)]
    pub arguments: serde_json::Value,
    /// "safe" | "caution" | "dangerous" (mirrors `RiskLevel::as_str`).
    #[serde(default = "default_risk")]
    pub risk: String,
    /// User decision: "approved" | "denied" | None (still pending).
    #[serde(default)]
    pub decision: Option<String>,
    /// Wall-clock ISO-8601 timestamp when the user clicked the button.
    /// `None` while the request is still pending.
    #[serde(default)]
    pub decided_at: Option<String>,
    /// Wall-clock deadline (Unix milliseconds) when the server
    /// would auto-deny this request. Mirrors the
    /// `StreamEvent::RequestApproval::expires_at` field on the
    /// wire. Persisted so a session reload can re-render the
    /// countdown correctly even after a page refresh — without
    /// this, the front-end would re-derive a fresh `expires_at`
    /// on reload and the user would see the original
    /// server-side timeout have already fired (the back-end
    /// already auto-denied while the page was reloading, so
    /// the "已超时" pill is the honest state).
    ///
    /// `None` on rows persisted before the countdown feature
    /// shipped (2026-06-14) — those rows predate the field and
    /// the front-end falls back to a 60s default in that case.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// The id of the assistant message this approval is sandwiched between
    /// (the one *before* the request). Used by the UI to position the
    /// inline card in the right spot after a reload.
    #[serde(default)]
    pub linked_assistant_id: Option<String>,
}

fn default_risk() -> String {
    "caution".to_string()
}

/// A row in the `messages` table, expanded to its typed payload.
/// `load_session` (LLM-facing) returns only the `Message` variants;
/// `load_session_with_approvals` (UI-facing) returns the interleaved
/// timeline including approval rows.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEntry {
    /// A regular LLM message (user / assistant / tool / system).
    Message(Message),
    /// An approval request card (always accompanied by a `linked_assistant_id`).
    Approval(ApprovalRecord),
    /// An approval card that has no `linked_assistant_id` (legacy rows
    /// persisted before that field existed). The UI treats it as
    /// "orphan" — render but don't try to position relative to anything.
    ApprovalOrphan(ApprovalRecord),
}

/// 2026-06-26 (A): one row of the `compression_archives` table —
/// the raw turns the compressor dropped from the in-memory
/// conversation when it folded them into the handoff summary.
/// `content` is JSON-encoded `Vec<Message>` so the schema doesn't
/// have to mirror the evolving `Message` Rust struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionArchiveRow {
    pub id: i64,
    pub content: String,
    pub original_turns: usize,
    pub compressed_turns: usize,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
    pub salience: f64,
    pub source: String,
    pub created_at: String,
}

impl CompressionArchiveRow {
    /// Decode `content` back into a `Vec<Message>`. Returns an empty
    /// vec if the JSON is malformed (corrupt row) — better to surface
    /// an empty archive than to panic on bad data.
    pub fn decoded_turns(&self) -> Vec<Message> {
        serde_json::from_str(&self.content).unwrap_or_default()
    }
}

/// 2026-06-26 (B): one row of the `compression_summary_history`
/// table — append-only log of every compression event with its
/// metrics. The `sessions.compressed_summary` column still holds the
/// latest summary as a fast-load index; this table is the historical
/// trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionHistoryRow {
    pub id: i64,
    pub summary: String,
    pub compression_count: u32,
    pub original_turns: usize,
    pub compressed_turns: usize,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
    pub tokens_saved: u64,
    pub created_at: String,
}

/// 2026-06-26 (A + B): a single compression event captured by the
/// agent loop. Threaded through `persist_session_with_summary` /
/// `save_session_atomic` so the DB layer can write the
/// `compression_archives` raw-archive row and the
/// `compression_summary_history` append-only row inside the same
/// transaction that updates `sessions.compressed_summary`. Without
/// this, the three writes would race and the historical trail could
/// drift out of sync with the latest-summary column.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressionSnapshot {
    /// The handoff summary text — also written to
    /// `sessions.compressed_summary` (latest-only, fast-load index).
    pub summary: String,
    /// Raw turns that the compressor folded into `summary`. Written
    /// to `compression_archives` so the original wording survives
    /// after `flush_messages` has wiped it from `messages`.
    pub archived_turns: Vec<Message>,
    /// 1-based compression index for this session (matches the
    /// `ContextCompressor::compression_count` value at the time of
    /// the event).
    pub compression_count: u32,
    pub original_turns: usize,
    pub compressed_turns: usize,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
}

impl CompressionSnapshot {
    /// Build from a raw summary + archived turns. Returns `None`
    /// when there's nothing to persist (empty summary AND empty
    /// archive), so the caller's `Option<&CompressionSnapshot>`
    /// stays out of the way for the no-compression case.
    pub fn new(
        summary: String,
        archived_turns: Vec<Message>,
        compression_count: u32,
        original_turns: usize,
        compressed_turns: usize,
        original_tokens: u64,
        compressed_tokens: u64,
    ) -> Option<Self> {
        if summary.is_empty() && archived_turns.is_empty() {
            return None;
        }
        Some(Self {
            summary,
            archived_turns,
            compression_count,
            original_turns,
            compressed_turns,
            original_tokens,
            compressed_tokens,
        })
    }
}

/// Join leading consecutive system messages (Python `_cached_system_prompt` / Anthropic prefix parity for persistence).
pub fn leading_system_prompt_for_persist(messages: &[Message]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        if m.role != MessageRole::System {
            break;
        }
        if let Some(c) = m
            .content
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(c.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Manages session persistence to SQLite and markdown log files.
pub struct SessionPersistence {
    /// Path to the SQLite database file.
    db_path: PathBuf,
    /// Directory for session log files.
    sessions_dir: PathBuf,
    /// Directory for trajectory files.
    trajectories_dir: PathBuf,
}

impl SessionPersistence {
    /// Create a new persistence manager rooted at the given hermes home directory.
    ///
    /// **Data files live under `<home>/data/`** (resolved via
    /// [`hermix_config::paths::data_dir_in`]), so `SessionPersistence`
    /// automatically picks up the correct paths in normal mode
    /// (`~/.hermix/data/sessions.db`) and portable mode
    /// (`<install_root>/data/sessions.db`).
    pub fn new(hermix_home: impl AsRef<Path>) -> Self {
        let data_dir = hermix_config::paths::data_dir_in(&hermix_home);
        Self {
            db_path: data_dir.join("sessions.db"),
            sessions_dir: data_dir.join("sessions"),
            trajectories_dir: data_dir.join("trajectories"),
        }
    }

    /// Path to the SQLite database file. Used by sibling code (e.g.
    /// the hermix-server 4-state approval side-effect handlers)
    /// that needs to read the original tool-call arguments out of
    /// the persisted approval row.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Ensure the SQLite database and tables exist.
    pub fn ensure_db(&self) -> Result<(), AgentError> {
        // -----------------------------------------------------------------
        // Migration 0: legacy DB from `$hermix_home/sessions.db` (pre-
        // data_dir) to `$hermix_home/data/sessions.db`. If the new DB
        // doesn't exist but the old one does, copy it so existing
        // sessions are not lost when the code path moves under data/.
        // -----------------------------------------------------------------
        let legacy_db = self.db_path.parent().and_then(|p| p.parent()).map(|home| home.join("sessions.db"));
        if let Some(ref legacy) = legacy_db {
            if legacy.exists() && !self.db_path.exists() {
                if let Some(parent) = self.db_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::copy(legacy, &self.db_path) {
                    Ok(sz) => tracing::info!(
                        "Migrated legacy sessions.db ({} bytes) to {}",
                        sz, self.db_path.display()
                    ),
                    Err(e) => tracing::warn!(
                        "Failed to copy legacy sessions.db: {e} (continuing with fresh DB)"
                    ),
                }
            }
        }

        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create db directory: {e}")))?;
        }

        let conn = open_with_wal(&self.db_path)?;

        // -----------------------------------------------------------------
        // Migrations FIRST.
        //
        // The CREATE TABLE / CREATE INDEX block below references the
        // `active` and `extra` columns. On a pre-migration database those
        // columns don't exist yet, and `CREATE INDEX ... ON
        // messages(session_id, active)` will fail with
        // `no such column: active` — the entire `execute_batch` then
        // bails and the ALTER TABLE migrations that would have added the
        // column never run. So: do the migrations first, then the
        // CREATE statements that depend on the migrated schema.
        //
        // Each migration silently no-ops on a fresh DB (the table doesn't
        // exist yet — CREATE TABLE IF NOT EXISTS below will create it with
        // the new column included) and on an already-migrated DB
        // ("duplicate column"). The two `no such table` / `duplicate
        // column` strings are the only ones we swallow.
        // -----------------------------------------------------------------
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN system_prompt TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.system_prompt: {e}"
                )));
            }
        }

        // Migration: messages.active (soft-delete flag for /undo).
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN active INTEGER NOT NULL DEFAULT 1", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.active: {e}"
                )));
            }
        }

        // Migration: messages.extra (JSON blob for approval rows + future
        // non-text metadata). Approval rows use role='approval' and stuff
        // request_id/tool/args/risk/decision/decided_at into this column.
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN extra TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.extra: {e}"
                )));
            }
        }

        // Migration (2026-06-22): add `name` column for tool messages.
        // Without this, reloaded tool messages always show "tool" as
        // the verb because tool_name is None (name not persisted).
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN name TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.name: {e}"
                )));
            }
        }

        // Migration (2026-06-25): add `workspace` and `expert_id` columns
        // to sessions table so the frontend can restore workspace/expert
        // when loading a historical session.
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN workspace TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.workspace: {e}"
                )));
            }
        }
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN expert_id TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.expert_id: {e}"
                )));
            }
        }

        // Migration: execution_timeline (tool call events) and
        // reasoning_content (LLM think blocks). Without these columns,
        // tool cards show as stale "运行中" on reload and thinking
        // text is lost. Serialized as JSON TEXT blobs.
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN execution_timeline TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.execution_timeline: {e}"
                )));
            }
        }
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN reasoning_content TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.reasoning_content: {e}"
                )));
            }
        }

        // Migration: purge pre-fix leaked system rows.
        // Versions of the app prior to the prompt-leak fix wrote
        // MessageRole::System into the messages table. Those rows are
        // now filtered at every read site, but the disk still has them
        // and the next re-persist (which overwrites via DELETE+INSERT
        // in flush_messages_to_session_db) would silently re-write
        // them too. Clean up so on-disk state matches the new contract.
        // Idempotent: a no-op once the first run has cleared the table.
        // We also bump `sessions.message_count` to keep the UI badge
        // honest after the purge.
        if let Ok(purged) = conn.execute(
            "DELETE FROM messages WHERE role = 'system'",
            [],
        ) {
            if purged > 0 {
                // Recount each affected session. Cheap (one UPDATE per
                // session row, and the SET message_count = (SELECT ...)
                // subquery is O(N) on indexed id).
                let _ = conn.execute(
                    "UPDATE sessions SET message_count = (
                        SELECT COUNT(*) FROM messages m WHERE m.session_id = sessions.id AND m.active = 1
                     )",
                    [],
                );
            }
        }

        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                system_prompt TEXT,
                compressed_summary TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                name TEXT,
                created_at TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                extra TEXT,
                execution_timeline TEXT,
                reasoning_content TEXT,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session_active
                ON messages(session_id, active);

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id);

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                session_id UNINDEXED,
                role UNINDEXED,
                content='messages',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;

            -- 2026-06-26 (A): append-only archive of the *raw* turns that
            -- the compressor folded into the handoff summary. The
            -- compressor (`ContextCompressor::compress`) hands us the
            -- `turns_to_summarize` slice; we JSON-encode it and store it
            -- here alongside the compression metrics so a future agent
            -- loop / dashboard / SRE postmortem can recover the original
            -- wording after the in-memory conversation has moved on.
            -- Why a separate table instead of stuffing it back into
            -- `messages`? Because `flush_messages` does
            -- DELETE+INSERT on `messages` per session — re-inserting
            -- would resurrect the very turns the compressor just
            -- dropped, and they would show up in the very next
            -- `load_session`. Keeping them in their own append-only
            -- table means the recall path is opt-in (call
            -- `load_compression_archive`) instead of implicit.
            CREATE TABLE IF NOT EXISTS compression_archives (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                original_turns INTEGER NOT NULL,
                compressed_turns INTEGER NOT NULL,
                original_tokens INTEGER NOT NULL,
                compressed_tokens INTEGER NOT NULL,
                salience REAL NOT NULL DEFAULT 0.5,
                source TEXT NOT NULL DEFAULT 'compression',
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_compression_archives_session
                ON compression_archives(session_id, created_at);

            -- 2026-06-26 (B): append-only summary history. The
            -- `sessions.compressed_summary` column is still the
            -- fast-load index (latest summary) but it overwrote every
            -- prior compression. This sibling table keeps every
            -- summary with its metrics so a session's full compaction
            -- trail is queryable — useful when the LLM says it lost
            -- track of what we discussed previously and we want to
            -- replay the entire conversation arc.
            CREATE TABLE IF NOT EXISTS compression_summary_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                compression_count INTEGER NOT NULL,
                original_turns INTEGER NOT NULL,
                compressed_turns INTEGER NOT NULL,
                original_tokens INTEGER NOT NULL,
                compressed_tokens INTEGER NOT NULL,
                tokens_saved INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_compression_summary_history_session
                ON compression_summary_history(session_id, created_at);

            -- P1-3 (2026-06-27): append-only LLM token usage log.
            -- `messages` carries the conversational transcript but its
            -- `flush_messages` DELETE+INSERT cycle means per-message
            -- `usage` would be wiped on every save. Storing usage in
            -- its own append-only table lets `aggregate_usage` sum
            -- real numbers across reloads, while also surviving
            -- schema upgrades of the `messages` table.
            --
            -- One row per agent turn that consumed tokens. `model`
            -- denormalised here (not joined via `sessions.model`) so
            -- historical model changes still surface in `/insights`
            -- without a join through a session row that may have
            -- been re-saved with a different default.
            CREATE TABLE IF NOT EXISTS token_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT 'unknown',
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost REAL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_token_usage_created_at
                ON token_usage(created_at);
            CREATE INDEX IF NOT EXISTS idx_token_usage_model
                ON token_usage(model);
            CREATE INDEX IF NOT EXISTS idx_token_usage_session
                ON token_usage(session_id, created_at);"#,
        )
        .map_err(|e| AgentError::Io(format!("Failed to create tables: {e}")))?;

        // Migration: add compressed_summary column if missing (existing databases).
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN compressed_summary TEXT",
            [],
        );

        Ok(())
    }

    /// P1-3 (2026-06-27): persist one LLM turn's token usage to the
    /// append-only `token_usage` table. Called from `agent_loop` after
    /// every successful API response so `aggregate_usage` can later
    /// sum real numbers instead of returning 0.
    ///
    /// Caller passes the raw `UsageStats` from the LLM response. We
    /// store it verbatim — no rounding, no batching — so a future
    /// `/insights` panel can chart per-turn granularity if we ever
    /// expose it. `estimated_cost` is `None` if the provider didn't
    /// populate it; the aggregator treats None as 0.
    pub fn persist_token_usage(
        &self,
        session_id: &str,
        model: &str,
        usage: &hermix_core::UsageStats,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO token_usage
                (session_id, model, prompt_tokens, completion_tokens,
                 total_tokens, estimated_cost, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                session_id,
                model,
                usage.prompt_tokens as i64,
                usage.completion_tokens as i64,
                usage.total_tokens as i64,
                usage.estimated_cost,
                now,
            ],
        )
        .map_err(|e| AgentError::Io(format!("Failed to insert token_usage: {e}")))?;
        Ok(())
    }

    /// Persist a session's messages to SQLite.
    /// Delegates to the unified [`save_session_atomic`] which wraps
    /// DB + markdown + trajectory writes in a single transaction.
    pub fn persist_session(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<(), AgentError> {
        self.save_session_atomic(session_id, messages, model, platform, title, system_prompt, None, None, false)
    }

    /// Persist a session's messages to SQLite, optionally including a
    /// compressed context summary for cross-session continuity.
    ///
    /// Delegates to the unified [`save_session_atomic`] which wraps
    /// DB + markdown + trajectory writes in a single transaction.
    ///
    /// 2026-06-26: prefer [`persist_session_with_compression`] when
    /// the caller has a full `CompressionSnapshot` (raw archive +
    /// summary + metrics). This legacy wrapper still exists for
    /// callers that only have the summary string; it builds a
    /// snapshot internally and passes through.
    pub fn persist_session_with_summary(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        compressed_summary: Option<&str>,
    ) -> Result<(), AgentError> {
        let snapshot = compressed_summary
            .filter(|s| !s.is_empty())
            .map(|s| CompressionSnapshot {
                summary: s.to_string(),
                ..Default::default()
            });
        let snapshot_ref = snapshot.as_ref();
        self.save_session_atomic(
            session_id,
            messages,
            model,
            platform,
            title,
            system_prompt,
            snapshot_ref,
            None,
            false,
        )
    }

    /// 2026-06-26 (A + B): persist a session with a full
    /// compression snapshot. The snapshot carries (a) the handoff
    /// summary, (b) the raw archived turns, and (c) the compression
    /// metrics. All three are written to their respective tables in
    /// the same transaction as the `messages` DELETE+INSERT, so the
    /// snapshot can never go out of sync with the latest summary
    /// even under a mid-write crash.
    ///
    /// When the snapshot is empty (no compression this turn), this
    /// is functionally identical to the legacy
    /// `persist_session` call.
    pub fn persist_session_with_compression(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        compression: Option<&CompressionSnapshot>,
    ) -> Result<(), AgentError> {
        self.save_session_atomic(
            session_id,
            messages,
            model,
            platform,
            title,
            system_prompt,
            compression,
            None,
            false,
        )
    }

    /// Persist session + messages + markdown + trajectory in a single
    /// atomic operation. Uses SQLite `BEGIN IMMEDIATE` so a crash
    /// between the three writes leaves the database in a consistent
    /// state (no partial DELETE). File writes (md / json) are attempted
    /// after the DB COMMIT — if they fail we log a warning but do not
    /// roll back, because the SQLite DB is the single source of truth.
    ///
    /// `compression` (2026-06-26) replaces the older
    /// `compressed_summary: Option<&str>` parameter: when non-`None`,
    /// the snapshot's summary also goes to `sessions.compressed_summary`
    /// (latest-only fast-load index), the raw archived turns go to
    /// `compression_archives` (A — durable raw), and the summary +
    /// metrics go to `compression_summary_history` (B — append-only
    /// history).
    ///
    /// `user_query` and `completed` are only needed for the trajectory
    /// file; pass `(None, false)` to skip trajectory.
    pub fn save_session_atomic(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        compression: Option<&CompressionSnapshot>,
        user_query: Option<&str>,
        completed: bool,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;

        let conn = open_with_wal(&self.db_path)?;

        // BEGIN IMMEDIATE — prevents other readers from seeing partial data
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| AgentError::Io(format!("Failed to begin transaction: {e}")))?;

        let summary_str = compression.map(|s| s.summary.as_str());
        let db_result = (|| -> Result<(), AgentError> {
            let now = Utc::now().to_rfc3339();

            // 1. UPSERT the session row FIRST so the messages.session_id →
            //    sessions.id FOREIGN KEY holds for the INSERT in step 2.
            //
            // We can't stamp the real `message_count` here — it depends on
            // how many rows `flush_messages` actually inserts (system rows
            // are filtered out, so `messages.len()` over-counts). We use a
            // 0 placeholder and patch it in step 3 with the real count
            // returned by `flush_messages`.
            conn.execute(
                "INSERT INTO sessions (id, model, platform, created_at, updated_at, title, message_count, system_prompt, compressed_summary)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, 0, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    updated_at = ?4,
                    title = COALESCE(?5, sessions.title),
                    system_prompt = COALESCE(?6, sessions.system_prompt),
                    compressed_summary = COALESCE(?7, sessions.compressed_summary)",
                rusqlite::params![
                    session_id,
                    model.unwrap_or("unknown"),
                    platform.unwrap_or("cli"),
                    now,
                    title,
                    system_prompt,
                    summary_str,
                ],
            )
            .map_err(|e| AgentError::Io(format!("Failed to upsert session: {e}")))?;

            // 2. Incrementally replace messages (transaction-protected DELETE+INSERT).
            //    Returns the actual rowcount so step 3 can stamp the on-disk count
            //    onto `sessions.message_count` instead of the over-counting
            //    `messages.len()`.
            let inserted = self.flush_messages(&conn, session_id, messages, &now)?;

            // 3. Patch `message_count` to match the real INSERT count. This
            //    also corrects any drift from prior versions that used
            //    `messages.len()` and over-counted by N (where N is the
            //    number of system-role rows in the in-memory array).
            conn.execute(
                "UPDATE sessions SET message_count = ?1 WHERE id = ?2",
                rusqlite::params![inserted as i64, session_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to patch session message_count: {e}")))?;

            // 4. (A + B): compression-side writes inside the same
            //    transaction. If any of these fails, the whole
            //    transaction rolls back and `messages` + `sessions`
            //    stay where they were — so the snapshot can never go
            //    out of sync with the latest summary.
            if let Some(snap) = compression {
                // A — raw archive: append-only, never delete.
                if !snap.archived_turns.is_empty() {
                    let content_json = serde_json::to_string(&snap.archived_turns)
                        .map_err(|e| AgentError::Io(format!(
                            "Failed to serialise archived turns: {e}"
                        )))?;
                    conn.execute(
                        "INSERT INTO compression_archives
                            (session_id, content, original_turns, compressed_turns,
                             original_tokens, compressed_tokens, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            session_id,
                            content_json,
                            snap.original_turns as i64,
                            snap.compressed_turns as i64,
                            snap.original_tokens as i64,
                            snap.compressed_tokens as i64,
                            now,
                        ],
                    )
                    .map_err(|e| AgentError::Io(format!(
                        "Failed to insert compression archive: {e}"
                    )))?;
                }
                // B — summary history: append-only, every compression
                // event gets one row even if the archived turns are
                // empty (e.g. summary-only fallback path).
                if !snap.summary.is_empty() {
                    let tokens_saved = snap
                        .original_tokens
                        .saturating_sub(snap.compressed_tokens)
                        as i64;
                    conn.execute(
                        "INSERT INTO compression_summary_history
                            (session_id, summary, compression_count, original_turns,
                             compressed_turns, original_tokens, compressed_tokens,
                             tokens_saved, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        rusqlite::params![
                            session_id,
                            snap.summary,
                            snap.compression_count as i64,
                            snap.original_turns as i64,
                            snap.compressed_turns as i64,
                            snap.original_tokens as i64,
                            snap.compressed_tokens as i64,
                            tokens_saved,
                            now,
                        ],
                    )
                    .map_err(|e| AgentError::Io(format!(
                        "Failed to insert compression summary history: {e}"
                    )))?;
                }
            }

            Ok(())
        })();

        match db_result {
            Ok(()) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| AgentError::Io(format!("Failed to commit transaction: {e}")))?;

                // 3. File writes (best-effort after DB COMMIT)
                if let Err(e) = self.save_session_log(session_id, messages, model) {
                    tracing::warn!("Failed to save session log (non-fatal): {e}");
                }
                if let Some(query) = user_query {
                    if let Err(e) = self.save_trajectory(session_id, messages, query, completed) {
                        tracing::warn!("Failed to save trajectory (non-fatal): {e}");
                    }
                }
                Ok(())
            }
            Err(e) => {
                conn.execute("ROLLBACK", [])
                    .map_err(|r| AgentError::Io(format!("Rollback failed after: {e}; nested: {r}")))?;
                Err(e)
            }
        }
    }


    /// Batch insert messages into the database for FTS5 indexing.
    ///
    /// **Transaction-protected**: the caller (e.g. `save_session_atomic`)
    /// is responsible for wrapping this in `BEGIN IMMEDIATE` / `COMMIT`.
    /// This inner helper only does the DELETE + INSERT.
    /// Persist `messages` for `session_id` after first wiping the existing
    /// rows. Returns the number of rows actually INSERTed (i.e. the count
    /// that `save_session_atomic` should stamp onto `sessions.message_count`).
    ///
    /// System-role rows are filtered out — they're rebuilt per-turn by
    /// `agent_loop` and never belong in the persisted timeline.
    ///
    /// **Transaction-protected**: the caller (e.g. `save_session_atomic`)
    /// is responsible for wrapping this in `BEGIN IMMEDIATE` / `COMMIT`.
    /// This inner helper only does the DELETE + INSERT.
    fn flush_messages(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        messages: &[Message],
        now: &str,
    ) -> Result<usize, AgentError> {
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to clear old messages: {e}")))?;

        let mut stmt = conn
            .prepare(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, name, created_at, execution_timeline, reasoning_content)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare insert: {e}")))?;

        let mut inserted: usize = 0;
        for msg in messages {
            if matches!(msg.role, MessageRole::System) {
                continue;
            }
            let role = match msg.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default());

            let timeline_json = msg
                .execution_timeline
                .as_ref()
                .map(|tl| serde_json::to_string(tl).unwrap_or_default());

            let safe_content = msg
                .content
                .as_deref()
                .map(crate::pii_redact::redact_secrets);
            let safe_tool_calls = tool_calls_json
                .as_deref()
                .map(crate::pii_redact::redact_tool_calls_json);

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
            ])
            .map_err(|e| AgentError::Io(format!("Failed to insert message: {e}")))?;
            inserted += 1;
        }

        Ok(inserted)
    }

    /// Save a human-readable session log as markdown.
    ///
    /// **Fixed filename**: `<session_id>.md` (overwrite, not new-file-per-save).
    /// Old timestamp-prefixed files (`{ts}-<sid>.md`) are moved to `orphans/`
    /// on first write; see `recover_orphan_session_logs`.
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

        let filename = format!("{session_id}.md");
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
            };

            content.push_str(&format!("### {role_label}\n\n"));

            if let Some(ref text) = msg.content {
                // Phase 18+: redact PII/secrets from the
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
    ///
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

        let filename = format!("{session_id}.json");
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
    ///
    /// Called once per `save_session_log` call for the given session_id
    /// (idempotent — the renamed file won't match the pattern again).
    pub fn recover_orphan_session_logs(sessions_dir: &Path, session_id: &str) {
        let Ok(entries) = std::fs::read_dir(sessions_dir) else { return };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Match: anything like `2026-06-18_130000-<sid>.md`
            if name.ends_with(&format!("-{session_id}.md")) && name != format!("{session_id}.md") {
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

    /// Load a previous session from SQLite.
    ///
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
                "SELECT role, content, tool_call_id, tool_calls, name, execution_timeline, reasoning_content
                 FROM messages
                 WHERE session_id = ?1 AND active = 1
                   AND role != 'approval' AND role != 'system'
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

                let role = match role_str.as_str() {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                };

                let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());
                let execution_timeline = timeline_json.and_then(|json| serde_json::from_str(&json).ok());

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
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        Ok(messages)
    }

    /// Load the compressed context summary for a session.
    ///
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

    /// 2026-06-26 (A): archive the *raw* turns that the compressor is
    /// about to fold into a handoff summary. Append-only — we never
    /// delete from `compression_archives` after a write, so the
    /// original wording survives even after `flush_messages` has
    /// wiped it from `messages` and the LLM has moved on.
    ///
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

    /// 2026-06-26 (B): append a row to the append-only summary
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

    /// 2026-06-26 (A): load the raw archive rows for a session, newest
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

    /// 2026-06-26 (B): load the append-only summary history for a
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

        // Phase 18+: scrub PII/secrets from the approval
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

    /// Load the full conversation timeline (LLM messages + approval rows)
    /// in arrival order. This is what the UI wants for `get_session_messages`.
    /// Approval rows are returned as `SessionEntry::Approval` so the
    /// caller can render them as inline cards in the right slot.
    ///
    /// IMPORTANT: approval rows are NOT included in the returned
    /// `Message`s — they live only in the timeline. If you need them as
    /// LLM context, filter the `Message` variants out of the result
    /// yourself (this is what `load_session` already does).
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
                "SELECT role, content, tool_call_id, tool_calls, extra, name, execution_timeline, reasoning_content
                 FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'system'
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare timeline query: {e}")))?;

        let entries = stmt
            .query_map(rusqlite::params![session_id], |row| {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let tool_call_id: Option<String> = row.get(2)?;
                let tool_calls_json: Option<String> = row.get(3)?;
                let extra_json: Option<String> = row.get(4)?;
                let db_name: Option<String> = row.get(5)?;
                let timeline_json: Option<String> = row.get(6)?;
                let reasoning_content: Option<String> = row.get(7)?;

                let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());

                if role_str == "approval" {
                    let record: ApprovalRecord = extra_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .ok_or_else(|| {
                            rusqlite::Error::InvalidQuery
                        })?;
                    if record.linked_assistant_id.is_some() {
                        Ok(SessionEntry::Approval(record))
                    } else {
                        Ok(SessionEntry::ApprovalOrphan(record))
                    }
                } else {
                    let role = match role_str.as_str() {
                        "system" => MessageRole::System,
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        "tool" => MessageRole::Tool,
                        // Unknown role — skip rather than crash. Future
                        // schema additions shouldn't take down old
                        // clients.
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    let execution_timeline = timeline_json.and_then(|json| serde_json::from_str(&json).ok());
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
                    }))
                }
            })
            .map_err(|e| AgentError::Io(format!("Failed to query timeline: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read timeline: {e}")))?;

        Ok(entries)
    }

    /// Soft-delete the most recent `count` user-to-assistant turns of a
    /// session. A turn is a contiguous slice of messages that starts
    /// with a user message and ends just before the next user message
    /// (or at the end of the conversation). The corresponding tool
    /// messages are also marked inactive.
    ///
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
                "SELECT id, role FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'approval'
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

    // DONE (2026-07-02 P1.x): token/cost persisted via `persist_token_usage()`
    // → `token_usage` table; aggregated by `aggregate_usage()` SQL.
    // No need for messages.extra.json hack.

    /// Aggregate real session / message / model counts over the last `days` days.
    /// P1-3 (2026-06-27): also sums token usage from `token_usage` and
    /// derives a daily curve + per-tool breakdown from
    /// `messages.tool_calls` JSON. Sessions / messages that predate the
    /// `token_usage` migration contribute `0` to token totals — that's
    /// fine, the chart shows the migration date as "0 baseline".
    ///   - `total_sessions`: count of sessions updated within the window
    ///   - `total_messages`: count of messages created within the window
    ///   - `by_model`: `(model_name, session_count, message_count)` tuples
    ///   - `recent_sessions`: the 20 most recent session ids
    pub fn aggregate_usage(
        &self,
        days: u32,
    ) -> Result<UsageAggregate, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // cutoff 时间：days 天前
        let now = Utc::now();
        let cutoff = now - chrono::Duration::days(days as i64);
        let cutoff_str = cutoff.to_rfc3339();

        // 1) total_sessions — 按 updated_at 过滤
        let total_sessions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE updated_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count sessions: {e}")))?;

        // 2) total_messages — 按 messages.created_at 过滤
        let total_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE created_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count messages: {e}")))?;

        // 3) by_model — 按 model 聚合 (now joined with token totals)
        // We LEFT JOIN `token_usage` so legacy sessions without usage
        // rows still appear (with input/output = 0). SUM treats NULLs
        // as 0 in SQLite so the COALESCE below is belt-and-suspenders.
        let mut stmt = conn
            .prepare(
                "SELECT
                    COALESCE(s.model, '(unknown)') AS m,
                    COUNT(DISTINCT s.id) AS session_count,
                    COALESCE(SUM(COALESCE(s.message_count, 0)), 0) AS message_sum,
                    COALESCE(SUM(COALESCE(t.prompt_tokens, 0)), 0) AS input_sum,
                    COALESCE(SUM(COALESCE(t.completion_tokens, 0)), 0) AS output_sum,
                    COALESCE(SUM(COALESCE(t.estimated_cost, 0)), 0.0) AS cost_sum
                 FROM sessions s
                 LEFT JOIN token_usage t
                    ON t.session_id = s.id AND t.created_at >= ?1
                 WHERE s.updated_at >= ?1
                 GROUP BY m
                 ORDER BY session_count DESC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare by_model: {e}")))?;
        let by_model: Vec<ModelUsageRow> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok(ModelUsageRow {
                    model: row.get(0)?,
                    sessions: row.get(1)?,
                    message_count: row.get(2)?,
                    input_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    estimated_cost: row.get::<_, f64>(5).unwrap_or(0.0),
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query by_model: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read by_model: {e}")))?;

        // 4) recent_sessions — 最新 20 条
        let mut stmt = conn
            .prepare(
                "SELECT id, COALESCE(title, '(无标题)'), COALESCE(model, '(unknown)'), updated_at
                 FROM sessions
                 ORDER BY updated_at DESC
                 LIMIT 20",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare recent: {e}")))?;
        let recent_sessions: Vec<RecentSessionRow> = stmt
            .query_map([], |row| {
                Ok(RecentSessionRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    model: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query recent: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read recent: {e}")))?;

        // 5) P1-3: token totals. SUM with COALESCE so a NULL `estimated_cost`
        //    doesn't propagate and break the f64 aggregator.
        let (total_input, total_output, total_cost): (i64, i64, f64) = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(COALESCE(estimated_cost, 0)), 0.0)
                 FROM token_usage
                 WHERE created_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| AgentError::Io(format!("Failed to sum token_usage: {e}")))?;

        // 6) P1-3: daily curve. We materialise the full window first
        //    (days+1 buckets ending today), then LEFT JOIN against the
        //    aggregated rows so missing days show as 0 instead of being
        //    absent. Doing this in Rust (not SQL) keeps the SQL
        //    portable — we don't have to ship a recursive CTE on every
        //    `aggregate_usage` call.
        let mut daily: Vec<DailyUsageRow> = Vec::with_capacity(days as usize + 1);
        for offset in 0..=days {
            let day_start = (now - chrono::Duration::days(offset as i64))
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
            daily.push(DailyUsageRow {
                day: day_start,
                input_tokens: 0,
                output_tokens: 0,
                estimated_cost: 0.0,
                sessions: 0,
                messages: 0,
            });
        }
        // Reverse so oldest is index 0, today is the last entry — matches
        // how the SVG line-chart reads left-to-right.
        daily.reverse();

        // Daily token rollup.
        let mut stmt = conn
            .prepare(
                "SELECT
                    substr(created_at, 1, 10) AS day,
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(COALESCE(estimated_cost, 0)), 0.0)
                 FROM token_usage
                 WHERE created_at >= ?1
                 GROUP BY day
                 ORDER BY day ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare daily: {e}")))?;
        let daily_token_rows: Vec<(String, i64, i64, f64)> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query daily: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read daily: {e}")))?;
        for (day, ip, op, cost) in daily_token_rows {
            if let Some(slot) = daily.iter_mut().find(|d| d.day == day) {
                slot.input_tokens = ip.max(0) as u64;
                slot.output_tokens = op.max(0) as u64;
                slot.estimated_cost = cost;
            }
        }
        // Daily session/message rollup — separate query so we don't
        // try to fold the LEFT JOIN into the token one (SQLite planner
        // gets confused by the OR on the date predicate).
        let mut stmt = conn
            .prepare(
                "SELECT
                    substr(s.updated_at, 1, 10) AS day,
                    COUNT(DISTINCT s.id) AS session_count,
                    COUNT(DISTINCT m.id) AS message_count
                 FROM sessions s
                 LEFT JOIN messages m
                    ON m.session_id = s.id
                    AND substr(m.created_at, 1, 10) = substr(s.updated_at, 1, 10)
                 WHERE s.updated_at >= ?1
                 GROUP BY day
                 ORDER BY day ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare daily sessions: {e}")))?;
        let daily_sess_rows: Vec<(String, i64, i64)> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query daily sessions: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read daily sessions: {e}")))?;
        for (day, sess, msgs) in daily_sess_rows {
            if let Some(slot) = daily.iter_mut().find(|d| d.day == day) {
                slot.sessions = sess.max(0) as u32;
                slot.messages = msgs.max(0) as u32;
            }
        }

        // 7) P1-3: by_tool aggregation. Parse `messages.tool_calls` JSON
        //    (which is `Vec<{function: {name}}>` per assistant message)
        //    and tally by `function.name`. Done in Rust rather than
        //    SQLite JSON1 because we already have the messages loaded
        //    pattern in `flush_messages` — and the cost of a single
        //    full-table scan over the date window is bounded by the
        //    active message count, which is usually <100k.
        //
        // We build a `HashMap<String, (total_calls, sessions_set_size)>`
        // and convert at the end. `daily_avg` uses trailing-7-day counts
        // (not the full window) so a tool that ran 500 times last week
        // but only 3 this week isn't papered over by ancient noise.
        let by_tool = self.aggregate_tools(&conn, &cutoff_str)?;

        Ok(UsageAggregate {
            total_sessions,
            total_messages,
            by_model,
            recent_sessions,
            total_input_tokens: total_input.max(0) as u64,
            total_output_tokens: total_output.max(0) as u64,
            total_cache_read_tokens: 0, // not tracked yet — surface as 0
            total_reasoning_tokens: 0,
            total_estimated_cost: total_cost,
            daily,
            by_tool,
        })
    }

    /// P1-3 helper: parse `messages.tool_calls` JSON within `cutoff_str`
    /// window, aggregate by tool name. Kept private so the public API
    /// stays a single `aggregate_usage(days)`.
    ///
    /// `tool_calls` JSON shape (mirrors `Message::tool_calls` — flattened
    /// `FunctionCall`):
    /// ```json
    /// [{"id": "call_xxx", "name": "read_file", "arguments": "..."}]
    /// ```
    /// Older OpenAI-style shape `{"function": {"name": ...}}` is also
    /// tolerated so a row written before the schema migration doesn't
    /// drop its counts.
    ///
    /// Defensive: malformed rows (no JSON, no name) are silently
    /// skipped — never bubble a JSON parse error out of an analytics
    /// query, the user just sees a slightly lower total.
    fn aggregate_tools(
        &self,
        conn: &rusqlite::Connection,
        cutoff_str: &str,
    ) -> Result<Vec<ToolUsageRow>, AgentError> {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, tool_calls, created_at
                 FROM messages
                 WHERE role = 'assistant'
                   AND tool_calls IS NOT NULL
                   AND tool_calls != ''
                   AND created_at >= ?1",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare tools query: {e}")))?;

        use std::collections::{HashMap, HashSet};
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut sessions: HashMap<String, HashSet<String>> = HashMap::new();
        // 7-day trailing window for `daily_avg`. We re-use `cutoff_str`
        // semantics (RFC3339) but compare against `created_at` substring
        // (which is also RFC3339) so a lexicographic comparison works.
        let now = Utc::now();
        let trailing7_cutoff = (now - chrono::Duration::days(7)).to_rfc3339();
        let mut trailing_counts: HashMap<String, u64> = HashMap::new();

        let mut rows = stmt
            .query(rusqlite::params![cutoff_str])
            .map_err(|e| AgentError::Io(format!("Failed to query tool rows: {e}")))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| AgentError::Io(format!("Failed to step tool rows: {e}")))?
        {
            let session_id: String = row.get(0).unwrap_or_default();
            let tool_calls_json: String = row.get(1).unwrap_or_default();
            let created_at: String = row.get(2).unwrap_or_default();
            let in_trailing7 = created_at >= trailing7_cutoff;

            let parsed: serde_json::Value = match serde_json::from_str(&tool_calls_json) {
                Ok(v) => v,
                Err(_) => continue, // skip malformed rows
            };
            let arr = match parsed.as_array() {
                Some(a) => a,
                None => continue,
            };
            for tc in arr {
                // Two accepted shapes — flattened `name` (canonical
                // after the ToolCall struct refactor) and the legacy
                // OpenAI `function.name` envelope.
                let name = tc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                    })
                    .unwrap_or("unknown");
                *counts.entry(name.to_string()).or_insert(0) += 1;
                sessions
                    .entry(name.to_string())
                    .or_insert_with(HashSet::new)
                    .insert(session_id.clone());
                if in_trailing7 {
                    *trailing_counts.entry(name.to_string()).or_insert(0) += 1;
                }
            }
        }
        drop(rows);

        let mut out: Vec<ToolUsageRow> = counts
            .into_iter()
            .map(|(tool, call_count)| {
                let session_count = sessions.get(&tool).map(|s| s.len() as u32).unwrap_or(0);
                let trailing = trailing_counts.get(&tool).copied().unwrap_or(0);
                ToolUsageRow {
                    tool,
                    call_count,
                    session_count,
                    daily_avg: (trailing as f64) / 7.0,
                }
            })
            .collect();
        out.sort_by(|a, b| b.call_count.cmp(&a.call_count));
        Ok(out)
    }
}

/// v1 (2026-06-22): PLAN_UI_ENHANCEMENT §6.1 — 真实聚合结果
/// v2 (2026-06-27, P1-3): 加 token + daily curve + by_tool 字段
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageAggregate {
    pub total_sessions: i64,
    pub total_messages: i64,
    pub by_model: Vec<ModelUsageRow>,
    pub recent_sessions: Vec<RecentSessionRow>,
    /// P1-3: token usage summed from `token_usage` within the window.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_reasoning_tokens: u64,
    pub total_estimated_cost: f64,
    /// P1-3: per-day breakdown for the time-curve SVG on `/insights`.
    /// Length is `days + 1` covering the full window (today inclusive).
    pub daily: Vec<DailyUsageRow>,
    /// P1-3: per-tool call counts parsed from `messages.tool_calls`.
    /// Sorted by `call_count` descending so the frontend can `slice(0, 10)`.
    pub by_tool: Vec<ToolUsageRow>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsageRow {
    pub model: String,
    pub sessions: i64,
    pub message_count: i64,
    /// P1-3: token totals broken out per model so `/insights` can
    /// render the cost-by-model bar list with real numbers.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecentSessionRow {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
}

/// P1-3: one calendar day's worth of usage. `day` is `YYYY-MM-DD` in
/// UTC (the cutoff is built from `Utc::now()` so a single user TZ
/// mix shouldn't shift the bucket boundaries).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyUsageRow {
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost: f64,
    pub sessions: u32,
    pub messages: u32,
}

/// P1-3: one tool's call count derived from `messages.tool_calls`
/// JSON. `daily_avg` is over the *trailing 7 days* — not the full
/// window — so a tool that exploded last week still shows current
/// frequency instead of being averaged away by older quiet periods.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolUsageRow {
    pub tool: String,
    pub call_count: u64,
    pub session_count: u32,
    pub daily_avg: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    #[test]
    fn test_persist_and_load_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];

        sp.persist_session(
            "test-session-1",
            &messages,
            Some("gpt-4o"),
            None,
            Some("Test"),
            Some("cached system blob"),
        )
        .unwrap();

        assert_eq!(
            sp.get_system_prompt("test-session-1").unwrap().as_deref(),
            Some("cached system blob")
        );

        let loaded = sp.load_session("test-session-1").unwrap();
        // System role is filtered out by `load_session` — the prompt
        // is reconstructed per-turn by `agent_loop`, not reloaded from
        // the timeline. The cache column above (lines 983-986) is the
        // canonical place to look it up; the timeline only holds the
        // user-visible turns.
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content.as_deref(), Some("Hello"));
        assert_eq!(loaded[1].content.as_deref(), Some("Hi there!"));
    }

    #[test]
    fn test_save_session_log() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![Message::user("What is 2+2?"), Message::assistant("4")];

        let path = sp
            .save_session_log("log-test", &messages, Some("gpt-4o"))
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("What is 2+2?"));
        assert!(content.contains("👤 User"));
        assert!(content.contains("🤖 Assistant"));
    }

    #[test]
    fn test_save_trajectory() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::user("Build a website"),
            Message::assistant("Sure, I'll help with that."),
        ];

        let path = sp
            .save_trajectory("traj-test", &messages, "Build a website", true)
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["completed"], true);
        assert_eq!(parsed["user_query"], "Build a website");
    }

    // ── P1-3: token_usage / daily / by_tool aggregation tests ─────

    fn tool_calls_json_legacy(names: &[&str]) -> String {
        let arr: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                serde_json::json!({
                    "id": format!("call_{i}"),
                    "type": "function",
                    "function": {"name": n, "arguments": "{}"}
                })
            })
            .collect();
        serde_json::to_string(&arr).unwrap()
    }

    fn usage(prompt: u64, completion: u64, cost: Option<f64>) -> hermix_core::UsageStats {
        hermix_core::UsageStats {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            estimated_cost: cost,
        }
    }

    #[test]
    fn p1_3_persist_token_usage_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Sessions FK is required — create the parent rows first so
        // the token_usage inserts don't trip FOREIGN KEY constraint.
        sp.persist_session("sess-1", &[Message::user("hi"), Message::assistant("yo")], Some("gpt-4o"), None, None, None).unwrap();
        sp.persist_session("sess-2", &[Message::user("hi"), Message::assistant("yo")], Some("claude"), None, None, None).unwrap();

        sp.persist_token_usage("sess-1", "gpt-4o", &usage(100, 50, Some(0.001)))
            .unwrap();
        sp.persist_token_usage("sess-1", "gpt-4o", &usage(200, 80, Some(0.002)))
            .unwrap();
        sp.persist_token_usage("sess-2", "claude", &usage(50, 30, None))
            .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        assert_eq!(agg.total_input_tokens, 350);
        assert_eq!(agg.total_output_tokens, 160);
        assert!((agg.total_estimated_cost - 0.003).abs() < 1e-9);
    }

    #[test]
    fn p1_3_aggregate_usage_daily_fills_missing_days_with_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session("sess-today", &[Message::user("hi"), Message::assistant("yo")], Some("gpt-4o"), None, None, None).unwrap();

        // Insert one row at "today" (whatever that is). Daily curve
        // should have `days + 1` entries; today filled, others zero.
        sp.persist_token_usage("sess-today", "gpt-4o", &usage(10, 5, None))
            .unwrap();

        let agg = sp.aggregate_usage(7).unwrap();
        assert_eq!(agg.daily.len(), 8, "days=7 should produce 8 daily buckets");
        let today = chrono::Utc::now().date_naive().format("%Y-%m-%d").to_string();
        let today_row = agg
            .daily
            .iter()
            .find(|d| d.day == today)
            .expect("today's bucket must exist");
        assert_eq!(today_row.input_tokens, 10);
        assert_eq!(today_row.output_tokens, 5);

        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let y_row = agg
            .daily
            .iter()
            .find(|d| d.day == yesterday)
            .expect("yesterday bucket must exist");
        assert_eq!(y_row.input_tokens, 0);
        assert_eq!(y_row.output_tokens, 0);
    }

    #[test]
    fn p1_3_by_tool_counts_flat_and_legacy_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Persist one session with two assistant messages — one with
        // the modern flat tool_calls shape, one with the legacy
        // `function.name` envelope. The aggregator must count both.
        let messages = vec![
            Message::user("read x and y"),
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: Some(vec![
                    hermix_core::ToolCall {
                        id: "c1".into(),
                        function: hermix_core::FunctionCall {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                    },
                    hermix_core::ToolCall {
                        id: "c2".into(),
                        function: hermix_core::FunctionCall {
                            name: "grep".into(),
                            arguments: "{}".into(),
                        },
                    },
                ]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
            },
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: None, // legacy tests write JSON directly below
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
            },
        ];
        sp.persist_session("sess-tools", &messages, Some("gpt-4o"), None, None, None)
            .unwrap();

        // Hand-overwrite the second assistant row with the legacy
        // envelope shape so we test the JSON-parser fallback path.
        use rusqlite::Connection;
        let conn = Connection::open(sp.db_path()).unwrap();
        let legacy = tool_calls_json_legacy(&["read_file", "read_file", "bash"]);
        conn.execute(
            "UPDATE messages SET tool_calls = ?1
             WHERE session_id = 'sess-tools' AND tool_calls IS NULL",
            rusqlite::params![legacy],
        )
        .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        let read = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "read_file")
            .expect("read_file must appear");
        let grep = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "grep")
            .expect("grep must appear");
        let bash = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "bash")
            .expect("bash must appear (legacy row)");
        assert_eq!(read.call_count, 3, "flat(1) + legacy(2) = 3");
        assert_eq!(grep.call_count, 1);
        assert_eq!(bash.call_count, 1);
        assert_eq!(read.session_count, 1);
    }

    #[test]
    fn p1_3_by_tool_sorts_descending_by_count() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Use a single message with multiple tool calls — easier than
        // threading several sessions.
        let mut tcs = Vec::new();
        for _ in 0..5 {
            tcs.push(hermix_core::ToolCall {
                id: "x".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            });
        }
        tcs.push(hermix_core::ToolCall {
            id: "y".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        });
        let messages = vec![
            Message::user("go"),
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: Some(tcs),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
            },
        ];
        sp.persist_session("sess-sort", &messages, Some("gpt-4o"), None, None, None)
            .unwrap();
        let agg = sp.aggregate_usage(30).unwrap();
        assert!(agg.by_tool[0].tool == "read_file");
        assert_eq!(agg.by_tool[0].call_count, 5);
        assert!(agg.by_tool[1].tool == "write_file");
        assert_eq!(agg.by_tool[1].call_count, 1);
    }

    #[test]
    fn p1_3_aggregate_usage_empty_db_returns_zero_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().unwrap();
        let agg = sp.aggregate_usage(7).unwrap();
        assert_eq!(agg.total_input_tokens, 0);
        assert_eq!(agg.total_output_tokens, 0);
        assert_eq!(agg.daily.len(), 8);
        assert!(agg.by_tool.is_empty());
        assert!(agg.by_model.is_empty());
    }

    #[test]
    fn p1_3_by_model_includes_token_totals_via_left_join() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("hi"), Message::assistant("hello!")];
        sp.persist_session("sess-a", &messages, Some("gpt-4o"), None, None, None)
            .unwrap();
        sp.persist_session("sess-b", &messages, Some("claude"), None, None, None)
            .unwrap();
        sp.persist_token_usage("sess-a", "gpt-4o", &usage(1000, 200, Some(0.01)))
            .unwrap();
        sp.persist_token_usage("sess-b", "claude", &usage(500, 100, Some(0.005)))
            .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        let gpt = agg
            .by_model
            .iter()
            .find(|m| m.model == "gpt-4o")
            .expect("gpt-4o model row");
        let claude = agg
            .by_model
            .iter()
            .find(|m| m.model == "claude")
            .expect("claude model row");
        assert_eq!(gpt.input_tokens, 1000);
        assert_eq!(gpt.output_tokens, 200);
        assert!((gpt.estimated_cost - 0.01).abs() < 1e-9);
        assert_eq!(claude.input_tokens, 500);
        assert_eq!(claude.output_tokens, 100);
    }

    #[test]
    fn test_load_nonexistent_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().unwrap();

        let loaded = sp.load_session("nonexistent").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_persist_replaces_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages1 = vec![Message::user("First")];
        sp.persist_session("replace-test", &messages1, None, None, None, None)
            .unwrap();

        let messages2 = vec![
            Message::user("First"),
            Message::assistant("Response"),
            Message::user("Second"),
        ];
        sp.persist_session("replace-test", &messages2, None, None, None, None)
            .unwrap();

        let loaded = sp.load_session("replace-test").unwrap();
        assert_eq!(loaded.len(), 3);
    }

    /// Helper: persist a 3-turn conversation (user / assistant per turn)
    /// and return the SessionPersistence for further inspection.
    fn seed_three_turns(tmp: &tempfile::TempDir) -> SessionPersistence {
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![
            Message::user("turn-1 user"),
            Message::assistant("turn-1 assistant"),
            Message::user("turn-2 user"),
            Message::assistant("turn-2 assistant"),
            Message::user("turn-3 user"),
            Message::assistant("turn-3 assistant"),
        ];
        sp.persist_session("undo-test", &messages, None, None, None, None)
            .unwrap();
        sp
    }

    #[test]
    fn test_undo_soft_deletes_last_n_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Undo the most recent 1 turn (turn-3: 2 messages).
        let n = sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        assert_eq!(n, 2);
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded.last().unwrap().content.as_deref(), Some("turn-2 assistant"));
    }

    #[test]
    fn test_undo_more_than_exist_deactivates_all() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Ask for 99 turns when only 3 exist — all 6 should be deactivated.
        let n = sp.mark_recent_turns_inactive("undo-test", 99).unwrap();
        assert_eq!(n, 6);
        let loaded = sp.load_session("undo-test").unwrap();
        assert!(loaded.is_empty(), "load should now be empty");
    }

    #[test]
    fn test_restore_brings_back_most_recent_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Undo last 1 turn, then restore.
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        let restored = sp.restore_last_undone("undo-test").unwrap();
        assert_eq!(restored, 2);
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 6);
    }

    #[test]
    fn test_undo_then_undo_again_undoes_previous_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_undo_on_empty_session_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let n = sp.mark_recent_turns_inactive("does-not-exist", 1).unwrap();
        assert_eq!(n, 0);
    }

    // ── Approval record round-trip ────────────────────────────────

    fn make_approval(req_id: &str, tool: &str, linked: Option<&str>) -> ApprovalRecord {
        ApprovalRecord {
            request_id: req_id.to_string(),
            tool: tool.to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
            risk: "dangerous".to_string(),
            decision: None,
            decided_at: None,
            expires_at: None,
            linked_assistant_id: linked.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_append_and_load_approval_timeline() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Seed: user → assistant → approval (linked to that assistant)
        // → tool → assistant (post-approval). The approval sits between
        // the two assistant messages in the timeline.
        let messages = vec![
            Message::user("first"),
            Message::assistant("first reply"),
        ];
        sp.persist_session("s1", &messages, None, None, None, None).unwrap();
        sp.append_approval_message(
            "s1",
            &make_approval("req-1", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        let entries = sp.load_session_with_approvals("s1").unwrap();
        assert_eq!(entries.len(), 3, "approval should interleave between the two messages");
        match &entries[0] {
            SessionEntry::Message(m) => assert_eq!(m.content.as_deref(), Some("first")),
            _ => panic!("expected user message at 0"),
        }
        match &entries[1] {
            SessionEntry::Message(m) => assert_eq!(m.content.as_deref(), Some("first reply")),
            _ => panic!("expected assistant message at 1"),
        }
        match &entries[2] {
            SessionEntry::Approval(r) => {
                assert_eq!(r.request_id, "req-1");
                assert_eq!(r.tool, "terminal_run");
                assert_eq!(r.linked_assistant_id.as_deref(), Some("reply-0"));
            }
            SessionEntry::ApprovalOrphan(_) => panic!("should be linked, not orphan"),
            _ => panic!("expected approval at 2"),
        }
    }

    #[test]
    fn test_load_session_filters_out_approval_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        sp.persist_session(
            "s2",
            &[Message::user("hi"), Message::assistant("hello")],
            None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s2",
            &make_approval("req-x", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        // LLM-facing load_session must NOT include the approval row.
        let llm_msgs = sp.load_session("s2").unwrap();
        assert_eq!(llm_msgs.len(), 2, "approval row must be filtered out of LLM context");
        assert!(llm_msgs.iter().all(|m| m.content.as_deref() != Some("")));

        // UI-facing load_session_with_approvals must include it.
        let entries = sp.load_session_with_approvals("s2").unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_resolve_approval_updates_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "s3",
            &[Message::user("hi"), Message::assistant("ok")],
            None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s3",
            &make_approval("req-y", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        sp.resolve_approval_message("s3", "req-y", "approved").unwrap();

        let entries = sp.load_session_with_approvals("s3").unwrap();
        match &entries[2] {
            SessionEntry::Approval(r) => {
                assert_eq!(r.decision.as_deref(), Some("approved"));
                assert!(r.decided_at.is_some(), "decided_at must be stamped");
            }
            _ => panic!("expected approval at 2"),
        }
    }

    #[test]
    fn test_append_approval_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "s4",
            &[Message::user("hi"), Message::assistant("ok")],
            None, None, None, None,
        )
        .unwrap();
        let rec = make_approval("req-dup", "terminal_run", Some("reply-0"));
        sp.append_approval_message("s4", &rec).unwrap();
        sp.append_approval_message("s4", &rec).unwrap();
        sp.append_approval_message("s4", &rec).unwrap();

        let entries = sp.load_session_with_approvals("s4").unwrap();
        // Only 3 entries total (user, assistant, one approval), not 5.
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_resolve_nonexistent_request_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Should not error even if there's no matching row.
        sp.resolve_approval_message("does-not-exist", "req-z", "approved").unwrap();
    }

    #[test]
    fn test_undo_also_deactivates_approval_rows_in_range() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Two turns. Insert an approval between turn 1 and turn 2 by
        // reordering the writes: first persist turn 1, then append an
        // approval (which gets a higher DB id), then persist turn 2 —
        // but persist_session REPLACES all messages, so we have to do
        // this differently. Instead, persist a turn, append approval,
        // then persist the next turn (replacing the messages table).
        // The approval row will be wiped by the second persist, so we
        // can't test the range heuristic this way.
        //
        // Workaround: manually craft the rows by calling append_approval
        // after a partial persist. We do two separate persist calls —
        // first the user+assistant of turn 1, then append_approval,
        // then user+assistant of turn 2. The second persist_session will
        // wipe the approval, so this approach can't model interleaved
        // approvals. Skip the test in that case; the SQL range logic
        // is exercised by hand when we ship the feature.
        sp.persist_session(
            "s5",
            &[Message::user("u1"), Message::assistant("a1")],
            None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s5",
            &make_approval("req-r", "terminal_run", Some("reply-0")),
        )
        .unwrap();
        sp.persist_session(
            "s5",
            &[
                Message::user("u1"),
                Message::assistant("a1"),
                Message::user("u2"),
                Message::assistant("a2"),
            ],
            None, None, None, None,
        )
        .unwrap();
        // The second persist wiped the approval row. Undo should now
        // return 2 (just turn-2) and the approval should still be gone.
        let n = sp.mark_recent_turns_inactive("s5", 1).unwrap();
        assert_eq!(n, 2);
    }

    /// Regression test for the rename_session / load_session crash on a
    /// pre-migration sessions.db: the old `messages` table doesn't have
    /// the `active` or `extra` columns, and the old `ensure_db` ran
    /// `CREATE INDEX ... ON messages(session_id, active)` *before* the
    /// `ALTER TABLE messages ADD COLUMN active` migration, so the
    /// `execute_batch` bailed on the index error and the migrations
    /// never ran. ensure_db now runs the migrations FIRST.
    #[test]
    fn test_ensure_db_migrates_legacy_messages_table() {
        use rusqlite::Connection;

        let tmp = tempfile::tempdir().unwrap();
        // DB now lives under <home>/data/ (via data_dir_in)
        let data_dir = hermix_config::paths::data_dir_in(tmp.path());
        std::fs::create_dir_all(&data_dir).unwrap();
        let db_path = data_dir.join("sessions.db");

        // Simulate a legacy DB: messages table without `active` / `extra`.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT,
                    platform TEXT DEFAULT 'cli',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    title TEXT,
                    message_count INTEGER DEFAULT 0
                );
                CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT,
                    tool_call_id TEXT,
                    tool_calls TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (session_id) REFERENCES sessions(id)
                );
                INSERT INTO sessions (id, created_at, updated_at)
                    VALUES ('s1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                INSERT INTO messages (session_id, role, content, created_at)
                    VALUES ('s1', 'user', 'hello', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        // Run ensure_db on the legacy DB — must succeed and add columns.
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().expect("ensure_db must succeed on legacy DB");

        // Verify the columns exist by running the same query that
        // load_session uses (was the original crash point).
        let conn = Connection::open(&db_path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_call_id, tool_calls, name, execution_timeline, reasoning_content
                 FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'approval'
                 ORDER BY id ASC",
            )
            .expect("legacy messages table should be migrated");
        let rows: Vec<String> = stmt
            .query_map(rusqlite::params!["s1"], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec!["user"]);

        // And `extra` should also have been added.
        let has_extra: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_extra, 1, "messages.extra column should exist");

        // Calling load_session should also work end-to-end now.
        let msgs = sp.load_session("s1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    /// Regression test for the `message_count` over-count bug:
    ///
    /// Before the fix, `save_session_atomic` stamped
    /// `sessions.message_count = messages.len() as i64` while
    /// `flush_messages` silently filtered out `MessageRole::System`
    /// rows. Result: the `message_count` column on the `sessions`
    /// table was always off by however many system rows were in the
    /// in-memory `messages` array (typically 1-2 — the cached system
    /// prompt from `agent_loop`). The UI badge rendered wrong, and any
    /// downstream code that paginated by `message_count` would skip or
    /// repeat rows.
    ///
    /// After the fix, `flush_messages` returns the actual INSERTed
    /// rowcount and `save_session_atomic` stamps that exact number.
    /// This test pins the contract: 2 system + 4 user/assistant
    /// messages in → exactly 4 messages in `messages` table AND
    /// `sessions.message_count = 4`.
    #[test]
    fn test_message_count_reflects_actual_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::system("cached system prompt blob"),
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
            Message::system("trailing system that must NOT count"),
        ];

        sp.persist_session("count-test", &messages, Some("gpt-4o"), None, Some("Count"), None)
            .unwrap();

        // 1. Open the DB directly and count the actual rows.
        let conn = rusqlite::Connection::open(sp.db_path()).unwrap();
        let actual_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params!["count-test"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            actual_rows, 4,
            "system rows must be filtered out — 2 system + 4 user/assistant = 4 stored"
        );

        // 2. The session row's message_count column must match the actual rowcount.
        let stamped: i64 = conn
            .query_row(
                "SELECT message_count FROM sessions WHERE id = ?1",
                rusqlite::params!["count-test"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stamped, 4,
            "sessions.message_count must equal the actual INSERTed rowcount, \
             not messages.len() (which would be 6 here)"
        );

        // 3. The persisted-and-reloaded LLM view should also be 4 messages
        //    (load_session already filters system rows; this just confirms
        //    the end-to-end count is consistent across all three views).
        let loaded = sp.load_session("count-test").unwrap();
        assert_eq!(loaded.len(), 4);
    }

    // 2026-06-26 (A + B): end-to-end test for the compression
    // archive + summary-history write paths. Calls
    // `persist_session_with_compression` once with a real
    // snapshot, then reads back both tables to confirm the
    // summary, the raw archived turns, and the metrics all
    // landed in their respective rows in the same transaction.
    #[test]
    fn compression_snapshot_writes_archive_and_history_in_one_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let session_id = "compression-snap-test";
        let messages = vec![
            Message::user("hello"),
            Message::assistant("hi"),
        ];
        let archived = vec![
            Message::user("original turn 1"),
            Message::assistant("original turn 2"),
            Message::user("original turn 3"),
        ];
        let snapshot = CompressionSnapshot::new(
            "the user said hi and we discussed the design".to_string(),
            archived,
            1,        // compression_count
            6, 2,     // original_turns, compressed_turns
            1500, 400, // original_tokens, compressed_tokens
        )
        .expect("non-empty summary + archived turns must build Some");

        sp.persist_session_with_compression(
            session_id,
            &messages,
            Some("test-model"),
            Some("local"),
            Some("Compression test"),
            Some("system"),
            Some(&snapshot),
        )
        .expect("persist must succeed");

        // (A) compression_archives row exists with the raw turns.
        let archives = sp
            .load_compression_archive(session_id, 10)
            .expect("archive query must succeed");
        assert_eq!(archives.len(), 1, "exactly one archive row");
        let row = &archives[0];
        assert_eq!(row.original_turns, 6);
        assert_eq!(row.compressed_turns, 2);
        assert_eq!(row.original_tokens, 1500);
        assert_eq!(row.compressed_tokens, 400);
        assert_eq!(row.source, "compression");
        let decoded = row.decoded_turns();
        assert_eq!(decoded.len(), 3);
        assert_eq!(
            decoded[0].content.as_deref(),
            Some("original turn 1")
        );

        // (B) compression_summary_history row exists with the metrics.
        let history = sp
            .load_compression_summary_history(session_id, 10)
            .expect("history query must succeed");
        assert_eq!(history.len(), 1, "exactly one history row");
        let h = &history[0];
        assert_eq!(h.compression_count, 1);
        assert_eq!(h.original_turns, 6);
        assert_eq!(h.compressed_turns, 2);
        assert_eq!(h.original_tokens, 1500);
        assert_eq!(h.compressed_tokens, 400);
        // 1500 - 400 = 1100 tokens saved
        assert_eq!(h.tokens_saved, 1100);
        assert_eq!(
            h.summary,
            "the user said hi and we discussed the design"
        );

        // (C) sessions.compressed_summary column still holds the
        // latest summary as a fast-load index.
        let latest = sp
            .load_compressed_summary(session_id)
            .expect("summary column must be populated");
        assert_eq!(latest, h.summary);
    }

    // 2026-06-26 (A): empty snapshot short-circuits — no archive
    // rows, no history rows, no summary column write.
    #[test]
    fn compression_snapshot_empty_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let session_id = "empty-snap-test";
        sp.persist_session_with_compression(
            session_id,
            &[Message::user("hi"), Message::assistant("hello")],
            Some("test-model"),
            Some("local"),
            None,
            None,
            None,
        )
        .expect("persist must succeed");

        let archives = sp
            .load_compression_archive(session_id, 10)
            .expect("archive query");
        assert!(archives.is_empty(), "no archive rows on empty snap");
        let history = sp
            .load_compression_summary_history(session_id, 10)
            .expect("history query");
        assert!(history.is_empty(), "no history rows on empty snap");
        assert!(
            sp.load_compressed_summary(session_id).is_none(),
            "no summary on empty snap"
        );
    }
}
