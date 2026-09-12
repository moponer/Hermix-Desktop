//! Session persistence — save and load conversation sessions.
//!
//! Provides SQLite-backed session storage with FTS5 indexing for search,
//! human-readable markdown session logs, and trajectory format for RL training.
//!
//! Corresponds to Python `run_agent.py`'s `_persist_session`, `_save_session_log`,
//! and `_save_trajectory` methods.

use std::path::{Path, PathBuf};

use hermix_core::{AgentError, Message, MessageRole};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Connection helper
// ---------------------------------------------------------------------------
// `rusqlite::Connection::open` defaults to `journal_mode = delete`
// (single writer, readers block writers) and `busy_timeout = 0`
// (zero retry on `SQLITE_BUSY`). With two processes or threads racing
// on the same DB — the agent loop and the dashboard status query, or
// two concurrent turns — the second opener gets `SQLITE_BUSY`
// immediately, and the `let _ = persist(...)` wrappers in the call
// sites silently swallow the error. The user sees the message
// "saved" but the row never lands in the DB.
// WAL allows concurrent readers + a single writer, and the busy
// timeout gives the writer 5 s to retry before giving up. WAL is
// also strictly more crash-resilient than the rollback journal.
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

/// Application-level retry with random jitter for write contention.
/// Mirrors Hermes Agent Python reference: 20-150ms jitter, up to 15 retries.
/// This catches contention that survives SQLite's busy_timeout (e.g. when
/// two processes race on BEGIN IMMEDIATE and the second opener's 5s window
/// expires before the first commits).
const WRITE_MAX_RETRIES: u32 = 15;
const WRITE_RETRY_MIN_MS: u64 = 20;
const WRITE_RETRY_MAX_MS: u64 = 150;

fn is_busy_error(e: &AgentError) -> bool {
    // Check all variants — SQLite errors can be wrapped in any AgentError
    // variant (Io, Config, Gateway, ToolExecution, etc.) depending on the
    // call site. Using the Display impl (via thiserror) ensures we catch
    // the underlying SQLite message regardless of how it was wrapped.
    let msg = e.to_string();
    msg.contains("SQLITE_BUSY") || msg.contains("database is locked")
}

/// Pseudo-random jitter for SQLITE_BUSY retry delays.
/// ### switched from xorshift64-on-static-counter to
/// `fastrand`.
/// The previous implementation kept a process-global `AtomicU64`
/// counter and ran xorshift64 on it. Two problems:
/// 1. **Predictability** — every process restart starts at the same
/// counter value, so retry-jitter patterns looked identical across
/// restarts. A stressed database + predictable jitter = collision
/// herd on the unlucky cycle.
/// 2. **Thread safety vs distribution quality** — xorshift64 has
/// decent statistical quality for a non-crypto PRNG but it isn't
/// thread-aware; multiple threads fetching `fetch_add` and running
/// the same polynomial produce correlated streams.
/// `fastrand` gives each thread its own (per-thread) auto-seeded RNG
/// with better distribution and zero atomic contention. Not crypto,
/// but jitter doesn't need to be.
/// Returns a uniformly distributed `u64` in `[0, u64::MAX]`.
fn rand_jitter_u64() -> u64 {
    fastrand::u64(..)
}

// ---------------------------------------------------------------------------
// SessionPersistence
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ApprovalRecord + SessionEntry
// ---------------------------------------------------------------------------
// Approval rows are stored in the same `messages` table as regular messages
// (so we get a single source of truth for the conversation timeline), but
// with `role='approval'`. The actual payload — request id, tool name,
// arguments, risk level, decision, and timestamp — lives in the `extra`
// JSON column (added in migration at `ensure_db` time).
// Why a row in `messages` rather than a separate `approvals` table?
// 1. Replay order: approval rows must interleave with assistant
// messages exactly when the agent emitted the `RequestApproval`
// event. Using a single `id` autoincrement PK + ORDER BY id ASC
// gives us that order for free.
// 2. Soft-delete parity: `/undo` and `/restore` already flip
// `active=0` on the relevant rows. Adding `WHERE role = 'approval'`
// to the existing helpers is a no-op for the LLM context (we filter
// approvals out before constructing `Message`s — see
// `load_session`), but their UI presence tracks the underlying
// conversation state naturally.
// 3. Single FTS5 trigger covers the table; no second trigger needed.

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
    /// `None` on rows persisted before the countdown feature
    /// shipped — those rows predate the field and
    /// the front-end falls back to a 60s default in that case.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// The id of the assistant message this approval is sandwiched between
    /// (the one *before* the request). Used by the UI to position the
    /// inline card in the right spot after a reload.
    #[serde(default)]
    pub linked_assistant_id: Option<String>,
    /// Backend-computed unified before/after diff for file-mutating calls
    /// (see `hermix_agent::approval_diff`). Persisted so a reloaded
    /// approval card shows the same diff the live card did. `None` for
    /// rows persisted before the field shipped and for call shapes that
    /// can't be diffed — the front-end keeps its plain-description
    /// fallback in that case.
    #[serde(default)]
    pub diff: Option<String>,
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

/// (A): one row of the `compression_archives` table —
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

/// (B): one row of the `compression_summary_history`
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

/// (A + B): a single compression event captured by the
/// agent loop. Threaded through `persist_session_with_summary` /
/// `save_session_atomic` so the DB layer can write the
/// `compression_archives` raw-archive row and the
/// `compression_summary_history` append-only row inside the same
/// transaction that updates `sessions.compressed_summary`. Without
/// this, the three writes would race and the historical trail could
/// drift out of sync with the latest-summary column.
/// (, Hermes 对标): compression now creates a real
/// child session row instead of in-place replacing the parent's
/// messages. The child has:
/// - `parent_session_id` pointing at the live session
/// - `compressed_summary` set to the handoff summary
/// - `messages = [system_role: <summary text>]` so a click in
/// the lineage tree shows what got folded
/// The parent session keeps its `messages` table untouched
/// (in-place behaviour preserved for UI routing — the user keeps
/// talking to the same session id), but it now has the child
/// reachable via `find_children(parent_session_id)` so the
/// frontend lineage tree can render the "before / after
/// compression" pivot.
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
    pub(crate) db_path: PathBuf,
    /// Directory for session log files.
    pub(crate) sessions_dir: PathBuf,
    /// Directory for trajectory files.
    pub(crate) trajectories_dir: PathBuf,
}



pub mod schema;
pub mod sessions;
pub mod messages;
pub mod usage;
#[cfg(test)]
mod tests;

fn estimate_tokens(s: &str) -> u64 {
    let mut tokens: f64 = 0.0;
    for ch in s.chars() {
        if ch.is_ascii() {
            tokens += 0.25;
        } else {
            tokens += 1.5;
        }
    }
    tokens.ceil() as u64
}

/// §6.1 — 真实聚合结果
/// v2 (, ): 加 token + daily curve + by_tool 字段
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageAggregate {
    pub total_sessions: i64,
    pub total_messages: i64,
    pub by_model: Vec<ModelUsageRow>,
    pub recent_sessions: Vec<RecentSessionRow>,
    /// token usage summed from `token_usage` within the window.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// now actually aggregated from `token_usage.cache_read_tokens`
    /// — the dashboard no longer hardcodes this to 0. Providers that
    /// don't report cache stats (MiniMax today) will still show 0.
    pub total_cache_read_tokens: u64,
    /// Anthropic-style `cache_creation_input_tokens`. Distinct
    /// from `cache_read_tokens` so the dashboard can chart "how much
    /// new prefix are we paying to cache" separately from "how much
    /// we're saving via cache hits".
    pub total_cache_creation_tokens: u64,
    /// extended-thinking / o1-style reasoning token totals.
    /// Surfaced in `/insights` so users can see how much of their
    /// spend is invisible to the visible-text token count.
    pub total_reasoning_tokens: u64,
    /// number of compression child sessions
    /// created within the window. Every auto/manual compression spawns
    /// a child session with `parent_session_id` set (), so this is
    /// a real, queryable "how many times did we compress" signal for
    /// the dashboard's compression card — not a synthetic counter.
    pub total_compactions: u64,
    /// total archived turns folded into
    /// compression summaries within the window. Counted as the messages
    /// inside compression-child sessions — those rows ARE the preserved
    /// history (we deliberately never insert the summary as a `system`
    /// message row, so every child message is an archived turn). This
    /// answers the user's "does compression lose my memory?" worry with
    /// a concrete number: how much history compression kept.
    pub total_archived_turns: u64,
    pub total_estimated_cost: f64,
    /// per-day breakdown for the time-curve SVG on `/insights`.
    /// Length is `days + 1` covering the full window (today inclusive).
    pub daily: Vec<DailyUsageRow>,
    /// per-tool call counts parsed from `messages.tool_calls`.
    /// Sorted by `call_count` descending so the frontend can `slice(0, 10)`.
    pub by_tool: Vec<ToolUsageRow>,
    /// per-skill usage (grouped by target skill name).
    pub skills: SkillUsageAggregate,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsageRow {
    pub model: String,
    pub sessions: i64,
    pub message_count: i64,
    /// token totals broken out per model so `/insights` can
    /// render the cost-by-model bar list with real numbers.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// cache + reasoning totals so each model's row can
    /// show its own cache hit rate and reasoning footprint, not
    /// just the global totals.
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_cost: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecentSessionRow {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
}

/// one calendar day's worth of usage. `day` is `YYYY-MM-DD` in
/// UTC (the cutoff is built from `Utc::now()` so a single user TZ
/// mix shouldn't shift the bucket boundaries).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyUsageRow {
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// cache + reasoning per-day. Lets the SVG daily curve
    /// layer cache-hit as a stacked band on top of input/output, so
    /// a "cache saved 60% of input tokens" day is visually obvious.
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_cost: f64,
    pub sessions: u32,
    pub messages: u32,
}

/// one tool's call count derived from `messages.tool_calls`
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

/// per-skill usage derived from `messages.tool_calls`
/// where `name` is `skill_view` (a "load"/view) or `skill_manage` (an
/// edit/create/delete). The skill name comes from the call's `arguments.name`.
/// Unlike `by_tool` (which counts the `skill_manage` *tool* once per call),
/// this groups by the *target skill* so the dashboard shows "code-review was
/// used 47 times", not "skill_manage ran 47 times".
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillUsageRow {
    pub skill: String,
    pub view_count: u64,
    pub manage_count: u64,
    pub last_used_at: Option<String>,
}

/// Aggregate of all skill usage within the window.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct SkillUsageAggregate {
    pub entries: Vec<SkillUsageRow>,
    pub total_views: u64,
    pub total_manages: u64,
    pub distinct_skills: u64,
}

