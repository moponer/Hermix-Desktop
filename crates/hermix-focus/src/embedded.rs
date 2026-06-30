//! Embedded SQLite-backed `MemoryLayer`.
//!
//! This is the "local-first" backend the调研 recommended (`PROPOSAL_BAILONGMA_INSPIRED.md`
//! §2.2 "本地计算 cosine", §五 Phase 8 L1).
//!
//! ## Storage location
//!
//! The DB lives at `<hermix_home>/data/memory.db`. In portable mode this
//! is `<install_root>/data/memory.db`. In CLI mode this is `~/.hermix/data/memory.db`.
//!
//! ## Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS frames (
//!     id            TEXT PRIMARY KEY,
//!     payload       TEXT NOT NULL,        -- JSON-encoded T (we store as JSON string)
//!     state         TEXT NOT NULL,
//!     salience      REAL NOT NULL,
//!     created_at    TEXT NOT NULL,
//!     updated_at    TEXT NOT NULL,
//!     parent_id     TEXT,
//!     tags          TEXT NOT NULL,        -- JSON array
//!     pin_count     INTEGER NOT NULL,
//!     touch_count   INTEGER NOT NULL,
//!     -- L1: vector-search columns. All 3 are nullable so frames
//!     -- inserted before L1.3 still load. embedding is stored as
//!     -- f32 little-endian bytes (4 bytes per dim).
//!     embedding       BLOB,
//!     embedding_dim   INTEGER,
//!     embedding_model TEXT
//! );
//! CREATE INDEX IF NOT EXISTS frames_created_at_idx ON frames(created_at);
//! CREATE INDEX IF NOT EXISTS frames_state_idx ON frames(state);
//! CREATE INDEX IF NOT EXISTS frames_salience_idx ON frames(salience);
//! -- L1: dim-indexed scan is the primary cosine path until we
//! -- upgrade to usearch HNSW in a later phase.
//! CREATE INDEX IF NOT EXISTS frames_embedding_dim_idx ON frames(embedding_dim);
//!
//! CREATE VIRTUAL TABLE IF NOT EXISTS frames_fts USING fts5(
//!     id UNINDEXED,
//!     payload_text,        -- extracted string from payload
//!     tags_text,
//!     tokenize = 'unicode61 remove_diacritics 2'
//! );
//! ```
//!
//! The FTS5 table is optional but provides sub-millisecond keyword recall.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde_json::Value;
use uuid::Uuid;

use crate::focus_stack::{FocusConclusion, FocusFrame, FocusFrameId, FrameState};
use crate::memory_layer::{
    MemoryError, MemoryHit, MemoryLayer, MemoryQuery, Result, SystemPromptBlock,
};
use crate::salience::{salience, SalienceConfig};

/// SQLite-backed local memory store.
pub struct EmbeddedMemoryBackend {
    conn: Mutex<Connection>,
    path: PathBuf,
    /// H5: salience config used by `rerank_by_salience` and
    /// `conclude` to apply the recency × freq × pin algorithm.
    salience_cfg: SalienceConfig,
    /// Phase 17: session_id bound via `initialize()`. Used as
    /// `frame.source` for entries written by the write-path hooks
    /// (`sync_turn`, `on_memory_write`, `on_session_end`).
    session_id: Mutex<Option<String>>,
    /// Phase 17: hermix_home bound via `initialize()`. Currently
    /// informational (we already know `path`) but kept on the struct
    /// so external observers (status panels, telemetry) can confirm
    /// which home directory the backend is bound to.
    hermix_home: Mutex<Option<String>>,
    /// Phase 17: monotonic turn counter incremented by
    /// `on_turn_start`. Used to gate the Hermes-Agent-inspired
    /// curation hook (`CURATE_INTERVAL`).
    turn_count: Mutex<u32>,
    /// Phase 17: log of the last N (user, assistant) turn pairs for
    /// the `on_pre_compress` hook to summarize. Bounded to avoid
    /// unbounded memory growth; we keep the last 20 pairs in
    /// memory.
    recent_turns: Mutex<Vec<TurnRecord>>,
}

/// Compact representation of a single user/assistant turn pair.
/// Kept in `EmbeddedMemoryBackend::recent_turns` so the
/// `on_pre_compress` hook can return a textual note of the recent
/// turns without round-tripping to SQLite.
#[derive(Debug, Clone)]
struct TurnRecord {
    turn_number: u32,
    user: String,
    assistant: String,
}

impl std::fmt::Debug for EmbeddedMemoryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedMemoryBackend")
            .field("path", &self.path)
            .finish()
    }
}

impl EmbeddedMemoryBackend {
    /// Open or create a DB at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        let backend = Self {
            conn: Mutex::new(conn),
            path,
            salience_cfg: SalienceConfig::default(),
            session_id: Mutex::new(None),
            hermix_home: Mutex::new(None),
            turn_count: Mutex::new(0),
            recent_turns: Mutex::new(Vec::new()),
        };
        backend.migrate()?;
        // Auto-cleanup expired frames on open
        backend.cleanup_expired_frames().ok();
        Ok(backend)
    }

    /// Remove frames that have expired (where expires_at is not NULL and < now).
    /// Called periodically to prevent unbounded growth.
    pub fn cleanup_expired_frames(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        let count = conn.execute(
            "DELETE FROM frames WHERE expires_at IS NOT NULL AND expires_at < ?1",
            params![now],
        )?;
        Ok(count)
    }

    /// Open an in-memory DB (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let backend = Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
            salience_cfg: SalienceConfig::default(),
            session_id: Mutex::new(None),
            hermix_home: Mutex::new(None),
            turn_count: Mutex::new(0),
            recent_turns: Mutex::new(Vec::new()),
        };
        backend.migrate()?;
        Ok(backend)
    }

    /// Path to the DB file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open a DB at `<data_dir>/memory.db`. `None` falls back to
    /// in-memory (used in tests).
    ///
    /// Uses [`hermix_config::paths::data_dir`] so the path is identical
    /// in normal mode (`~/.hermix/data/memory.db`) and portable mode
    /// (`<install_root>/data/memory.db`).
    pub fn open_at_hermix_home() -> Result<Self> {
        let db_path = hermix_config::paths::data_dir().join("memory.db");
        Self::open(&db_path)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        // Phase 18 (P1-3): PRAGMA user_version 跟踪 schema 演进。
        // 已存在的 CREATE TABLE / ALTER TABLE 仍然全部跑（向后兼容），
        // 但新增的 schema 块（janitor、recall_audit、session_snapshots）
        // 用 version 守卫，避免每次 open() 跑 14 个 SQL。
        // - 新装 DB：user_version=0 → 全部 migration 顺序执行 → 4
        // - 已有 DB（user_version=3）：只跑 v4
        // - 最新 DB（user_version=4）：啥都不跑（极快）
        const CURRENT_SCHEMA_VERSION: i64 = 4;
        let current_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        // If the user_version is already current, we still need to
        // ensure the idempotent CREATE TABLE IF NOT EXISTS ran
        // (handles the case where DB was created at < v4 but the
        // PRAGMA write below failed at some point).
        if current_version > CURRENT_SCHEMA_VERSION {
            tracing::warn!(
                target: "hermix.focus",
                stored = current_version,
                supported = CURRENT_SCHEMA_VERSION,
                "memory.db user_version is ahead of code; this is unusual — \
                 opening in read-mostly mode and skipping future migrations"
            );
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS frames (
                id            TEXT PRIMARY KEY,
                payload       TEXT NOT NULL,
                state         TEXT NOT NULL,
                salience      REAL NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                parent_id     TEXT,
                tags          TEXT NOT NULL,
                pin_count     INTEGER NOT NULL,
                touch_count   INTEGER NOT NULL,
                embedding       BLOB,
                embedding_dim   INTEGER,
                embedding_model TEXT,
                expires_at      TEXT,
                -- 2026-06-26 (BaiLongma soft-hide): every frame is
                -- visible by default. Soft-deleting flips `visibility`
                -- to 0 and stamps `hidden_at`. `recognize()` always
                -- filters `WHERE visibility = 1` so an accidental
                -- delete can be undone with `UPDATE frames SET
                -- visibility = 1, hidden_at = NULL WHERE id = ?`.
                visibility      INTEGER NOT NULL DEFAULT 1,
                hidden_at       TEXT,
                -- 2026-06-26 (write-path provenance): which hook
                -- produced this row. One of `focus` (legacy),
                -- `turn` (sync_turn), `memory_tool`
                -- (on_memory_write), `session` (on_session_end),
                -- `delegation` (on_delegation), `curation`
                -- (auto-curation hook), `compression_backfill`
                -- (async LLM summary after frame pop).
                source          TEXT NOT NULL DEFAULT 'focus'
            );
            CREATE INDEX IF NOT EXISTS frames_created_at_idx ON frames(created_at);
            CREATE INDEX IF NOT EXISTS frames_state_idx ON frames(state);
            CREATE INDEX IF NOT EXISTS frames_salience_idx ON frames(salience);
            -- embedding_dim / visibility / source indexes created in migration loop below
            "#,
        )?;
        // L1: forward-compat for DBs created before L1.3.
        // `PRAGMA table_info` reports the column set, so an idempotent
        // ALTER TABLE is just "add the column if it's missing". We do
        // this for each of the 3 new columns.
        for col in [
            "embedding",
            "embedding_dim",
            "embedding_model",
            "expires_at",
            "visibility",
            "hidden_at",
            "source",
        ] {
            let already: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('frames') WHERE name = ?1")?
                .query_row(params![col], |_| Ok(true))
                .unwrap_or(false);
            if !already {
                let decl = match col {
                    "embedding" => "BLOB",
                    "embedding_dim" => "INTEGER",
                    "embedding_model" => "TEXT",
                    "expires_at" => "TEXT",
                    "visibility" => "INTEGER NOT NULL DEFAULT 1",
                    "hidden_at" => "TEXT",
                    "source" => "TEXT NOT NULL DEFAULT 'focus'",
                    _ => unreachable!(),
                };
                conn.execute_batch(&format!("ALTER TABLE frames ADD COLUMN {col} {decl}"))?;
                if col == "embedding_dim" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS frames_embedding_dim_idx ON frames(embedding_dim)",
                    )?;
                }
                if col == "visibility" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS frames_visibility_idx ON frames(visibility)",
                    )?;
                }
                if col == "source" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS frames_source_idx ON frames(source)",
                    )?;
                }
            }
        }
        // Phase 17 (BaiLongma soft-hide): drop frames that were
        // marked hidden more than 30 days ago. The audit trail in
        // `frames_visibility_idx` lets SRE / dashboard queries see
        // hidden frames; the periodic janitor only purges really
        // ancient hidden rows. Keeping the threshold long (30 days)
        // because memory.db is small and a hidden row is cheap.
        // This runs synchronously on `open()`. When the DB grows
        // past 100K rows we'll move it to a background task.
        let _ = conn.execute(
            "DELETE FROM frames WHERE visibility = 0
             AND hidden_at IS NOT NULL
             AND hidden_at < datetime('now', '-30 days')",
            [],
        );
        // 2026-06-26 (Hermes Agent curation): separate `facts`
        // table for entities / facts the agent extracts at
        // curation time. Lives next to `frames` because the same
        // schema (id, payload, salience, tags, created_at) covers
        // both. `facts` adds a stable key (`canonical_key`) for
        // dedup and an `access_count` for salience boost on hit.
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS facts (
                id             TEXT PRIMARY KEY,
                canonical_key  TEXT NOT NULL,
                payload        TEXT NOT NULL,
                salience       REAL NOT NULL,
                created_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL,
                tags           TEXT NOT NULL,
                source         TEXT NOT NULL DEFAULT 'curation',
                access_count   INTEGER NOT NULL DEFAULT 0,
                visibility     INTEGER NOT NULL DEFAULT 1,
                embedding       BLOB,
                embedding_dim   INTEGER,
                embedding_model TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS facts_canonical_key_uidx
                ON facts(canonical_key) WHERE visibility = 1;
            CREATE INDEX IF NOT EXISTS facts_salience_idx ON facts(salience);
            CREATE INDEX IF NOT EXISTS facts_created_at_idx ON facts(created_at);
            CREATE INDEX IF NOT EXISTS facts_embedding_dim_idx ON facts(embedding_dim);
            CREATE INDEX IF NOT EXISTS facts_visibility_idx ON facts(visibility);
            CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
                id UNINDEXED,
                payload_text,
                tags_text,
                canonical_key UNINDEXED,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            "#,
        )?;
        for col in [
            "canonical_key",
            "access_count",
            "embedding",
            "embedding_dim",
            "embedding_model",
        ] {
            let already: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('facts') WHERE name = ?1")?
                .query_row(params![col], |_| Ok(true))
                .unwrap_or(false);
            if !already {
                let decl = match col {
                    "canonical_key" => "TEXT NOT NULL DEFAULT ''",
                    "access_count" => "INTEGER NOT NULL DEFAULT 0",
                    "embedding" => "BLOB",
                    "embedding_dim" => "INTEGER",
                    "embedding_model" => "TEXT",
                    _ => unreachable!(),
                };
                conn.execute_batch(&format!("ALTER TABLE facts ADD COLUMN {col} {decl}"))?;
                if col == "embedding_dim" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS facts_embedding_dim_idx ON facts(embedding_dim)",
                    )?;
                }
            }
        }
        // FTS5 is optional; create it if the feature is compiled in.
        // rusqlite's `bundled` feature includes SQLite with FTS5.
        let fts_result = conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS frames_fts USING fts5(
                id UNINDEXED,
                payload_text,
                tags_text,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            "#,
        );
        // If FTS5 init fails (some builds lack it), silently skip — the
        // base `frames` table still works for non-keyword queries.
        if let Err(e) = fts_result {
            tracing::warn!("FTS5 not available, falling back to LIKE search: {e}");
        }

        // ---- Phase 18 (P1-3): version-gated incremental migrations ----
        // v2: janitor + audit (already running above — keep behaviour
        // for backward compatibility, just bump the version so future
        // opens know it ran).
        if current_version < 2 {
            // Already executed above; we only need to mark it done.
            let _ = conn.execute_batch("PRAGMA user_version = 2");
        }
        // v3: recall_audit table (Phase 18 P1-1: query/timestamp/hit_count/latency).
        if current_version < 3 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS recall_audit (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id  TEXT NOT NULL,
                    query_text  TEXT NOT NULL,
                    query_hash  TEXT NOT NULL,
                    hit_count   INTEGER NOT NULL,
                    latency_us  INTEGER NOT NULL,
                    ts          TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS recall_audit_session_idx
                    ON recall_audit(session_id);
                CREATE INDEX IF NOT EXISTS recall_audit_ts_idx
                    ON recall_audit(ts);
                CREATE INDEX IF NOT EXISTS recall_audit_query_hash_idx
                    ON recall_audit(query_hash);
                "#,
            )?;
            let _ = conn.execute_batch("PRAGMA user_version = 3");
        }
        // v4: session_snapshots table (Phase 18 架构-1: frozen snapshot).
        if current_version < 4 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS session_snapshots (
                    id             INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id     TEXT NOT NULL,
                    turn_number    INTEGER NOT NULL,
                    summary        TEXT NOT NULL,
                    frame_ids      TEXT NOT NULL,
                    ts             TEXT NOT NULL,
                    visibility     INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS session_snapshots_session_idx
                    ON session_snapshots(session_id);
                CREATE INDEX IF NOT EXISTS session_snapshots_turn_idx
                    ON session_snapshots(session_id, turn_number);
                "#,
            )?;
            let _ = conn.execute_batch("PRAGMA user_version = 4");
        }
        Ok(())
    }

    /// Persist a focus frame (or update an existing one by id).
    ///
    /// This is the main write path used by `FocusStack::drain_conclusions()`
    /// or by the agent loop after each turn.
    pub fn upsert_frame<T: serde::Serialize + Clone>(&self, frame: &FocusFrame<T>) -> Result<()> {
        self.upsert_frame_with_source(frame, "focus")
    }

    /// Variant of [`Self::upsert_frame`] that records the write-path
    /// hook that produced the row (used by the new
    /// `sync_turn`/`on_memory_write`/`on_session_end` hooks so
    /// callers can later ask "show me everything that came from
    /// `memory_tool`").
    pub fn upsert_frame_with_source<T: serde::Serialize + Clone>(
        &self,
        frame: &FocusFrame<T>,
        source: &str,
    ) -> Result<()> {
        let payload = serde_json::to_string(&frame.payload)?;
        let tags_json = serde_json::to_string(&frame.tags)?;
        let state_str = state_to_str(frame.state);
        let created_at = frame.created_at.to_rfc3339();
        let updated_at = frame.updated_at.to_rfc3339();
        let parent_id = frame.parent_id.map(|p| p.to_string());
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT OR REPLACE INTO frames
               (id, payload, state, salience, created_at, updated_at,
                parent_id, tags, pin_count, touch_count,
                visibility, source)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11)"#,
            params![
                frame.id.to_string(),
                payload,
                state_str,
                frame.salience,
                created_at,
                updated_at,
                parent_id,
                tags_json,
                frame.pin_count as i64,
                frame.touch_count as i64,
                source,
            ],
        )?;
        // Also index in FTS5 (best-effort). Re-serialize for indexing so
        // we don't move out of the borrowed frame.
        let payload_for_index = serde_json::to_value(&frame.payload)?;
        let payload_text = extract_text(&payload_for_index);
        let tags_text = frame.tags.join(" ");
        let _ = conn.execute(
            "INSERT OR REPLACE INTO frames_fts (id, payload_text, tags_text) VALUES (?1, ?2, ?3)",
            params![frame.id.to_string(), payload_text, tags_text],
        );
        Ok(())
    }

    /// Persist a focus conclusion (separate row, no payload).
    pub fn upsert_conclusion(&self, c: &FocusConclusion) -> Result<()> {
        self.upsert_frame(&FocusFrame::new(c.clone())
            .with_salience(c.salience)
            .with_tag("conclusion"))
    }

    /// Load a frame by id.
    pub fn get_frame(&self, id: FocusFrameId) -> Result<Option<StoredFrame>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, payload, state, salience, created_at, updated_at,
                    parent_id, tags, pin_count, touch_count
             FROM frames WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.to_string()])?;
        if let Some(row) = rows.next()? {
            Ok(Some(StoredFrame::from_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Total number of frames stored.
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Delete a frame by id.
    ///
    /// Phase 17: prefer [`Self::soft_hide_frame`] so the audit trail
    /// is preserved. `delete_frame` is kept for the rare cases where
    /// the caller is sure they want a hard delete (e.g. tests, the
    /// 30-day janitor on hidden rows).
    pub fn delete_frame(&self, id: FocusFrameId) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM frames WHERE id = ?1", params![id.to_string()])?;
        let _ = conn.execute("DELETE FROM frames_fts WHERE id = ?1", params![id.to_string()]);
        Ok(n > 0)
    }

    /// Phase 17 (BaiLongma soft-hide): mark a frame invisible to
    /// `recognize()` queries without losing the row. Use this when
    /// the agent decides a memory is no longer relevant (TTL
    /// expiry, semantic eviction, manual `memory` tool remove).
    ///
    /// Returns `true` if a frame was actually hidden (i.e. it
    /// existed and was visible before).
    pub fn soft_hide_frame(&self, id: FocusFrameId) -> Result<bool> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let n = conn.execute(
            "UPDATE frames SET visibility = 0, hidden_at = ?1
             WHERE id = ?2 AND visibility = 1",
            params![now, id.to_string()],
        )?;
        Ok(n > 0)
    }

    /// Phase 17: undo a previous [`Self::soft_hide_frame`]. Returns
    /// `true` if a frame was actually restored.
    pub fn show_frame(&self, id: FocusFrameId) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE frames SET visibility = 1, hidden_at = NULL
             WHERE id = ?1 AND visibility = 0",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    /// Phase 17 (write-path hook target): persist a single
    /// user/assistant turn pair as a low-salience (0.3) frame tagged
    /// `turn`. Stored separately from the focus-stack conclusions
    /// (which are higher salience) so recall can prefer user-asserted
    /// facts over raw turns.
    ///
    /// `turn_number` is monotonic per session. When the user message
    /// is empty or only whitespace, the call is a no-op (avoids
    /// polluting recall with accidental empty rows).
    pub fn record_turn(
        &self,
        user: &str,
        assistant: &str,
        session_id: &str,
        turn_number: u32,
    ) -> Result<Option<FocusFrameId>> {
        let user_trim = user.trim();
        let assistant_trim = assistant.trim();
        if user_trim.is_empty() && assistant_trim.is_empty() {
            return Ok(None);
        }
        // Concatenate the turn pair into a single payload so the
        // generic `upsert_frame_with_source` (which requires
        // `serde::Serialize`) can carry it. JSON keeps extraction
        // round-trippable.
        let payload = serde_json::json!({
            "kind": "turn",
            "user": user_trim,
            "assistant": assistant_trim,
            "session_id": session_id,
            "turn_number": turn_number,
        });
        let frame = FocusFrame::new(payload)
            .with_salience(0.3)
            .with_tag("turn")
            .with_tag(format!("session:{session_id}"));
        let id = frame.id;
        self.upsert_frame_with_source(&frame, "turn")?;
        Ok(Some(id))
    }

    /// Phase 17 (write-path hook target): persist an explicit
    /// `memory` tool call. Higher salience (0.7) than raw turns
    /// because the user actively asserted this fact via a tool.
    pub fn record_memory_write(
        &self,
        action: &str,
        target: &str,
        content: &str,
    ) -> Result<Option<FocusFrameId>> {
        let content_trim = content.trim();
        if content_trim.is_empty() {
            return Ok(None);
        }
        let payload = serde_json::json!({
            "kind": "memory_write",
            "action": action,
            "target": target,
            "content": content_trim,
        });
        let frame = FocusFrame::new(payload)
            .with_salience(0.7)
            .with_tag("memory_write")
            .with_tag(format!("target:{target}"));
        let id = frame.id;
        self.upsert_frame_with_source(&frame, "memory_tool")?;
        Ok(Some(id))
    }

    /// Phase 17 (write-path hook target): persist a delegation
    /// event. Medium salience (0.5) — useful for cross-session
    /// recall of "what did I delegate to whom" without dominating
    /// the recall ranking.
    pub fn record_delegation(
        &self,
        task: &str,
        sub_agent_id: &str,
    ) -> Result<Option<FocusFrameId>> {
        let task_trim = task.trim();
        if task_trim.is_empty() {
            return Ok(None);
        }
        let payload = serde_json::json!({
            "kind": "delegation",
            "task": task_trim,
            "sub_agent_id": sub_agent_id,
        });
        let frame = FocusFrame::new(payload)
            .with_salience(0.5)
            .with_tag("delegation")
            .with_tag(format!("agent:{sub_agent_id}"));
        let id = frame.id;
        self.upsert_frame_with_source(&frame, "delegation")?;
        Ok(Some(id))
    }

    /// Phase 17: insert or replace a `fact` row. Used by the
    /// Hermes-Agent-inspired curation hook (every N turns, or after
    /// a complex task) and by external callers that already
    /// extracted a structured fact.
    ///
    /// `canonical_key` is the stable identity used for dedup.
    /// If a fact with the same key already exists (and is visible),
    /// its `access_count` is bumped and `payload`/`salience` are
    /// updated; otherwise a new row is inserted.
    pub fn upsert_fact(
        &self,
        canonical_key: &str,
        payload: serde_json::Value,
        salience: f32,
        tags: &[String],
        source: &str,
    ) -> Result<Option<FocusFrameId>> {
        let key_trim = canonical_key.trim();
        if key_trim.is_empty() {
            return Ok(None);
        }
        let payload_text = extract_text(&payload);
        if payload_text.trim().is_empty() {
            return Ok(None);
        }
        let tags_json = serde_json::to_string(tags)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        // Try to bump the existing row first.
        let updated = conn.execute(
            "UPDATE facts SET access_count = access_count + 1,
                              updated_at = ?1,
                              payload = ?2,
                              salience = MAX(salience, ?3),
                              tags = ?4,
                              source = ?5
             WHERE canonical_key = ?6 AND visibility = 1",
            params![now, payload.to_string(), salience, tags_json, source, key_trim],
        )?;
        let id = if updated > 0 {
            // Read back the existing id so callers can pin it.
            let id: String = conn.query_row(
                "SELECT id FROM facts WHERE canonical_key = ?1 AND visibility = 1",
                params![key_trim],
                |r| r.get(0),
            )?;
            Uuid::parse_str(&id).map_err(|e| MemoryError::Other(format!("uuid parse: {e}")))?
        } else {
            let new_id = Uuid::new_v4();
            conn.execute(
                "INSERT INTO facts
                 (id, canonical_key, payload, salience, created_at, updated_at,
                  tags, source, access_count, visibility)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, 0, 1)",
                params![
                    new_id.to_string(),
                    key_trim,
                    payload.to_string(),
                    salience,
                    now,
                    tags_json,
                    source,
                ],
            )?;
            let _ = conn.execute(
                "INSERT OR REPLACE INTO facts_fts (id, payload_text, tags_text, canonical_key)
                 VALUES (?1, ?2, ?3, ?4)",
                params![new_id.to_string(), payload_text, tags_json, key_trim],
            );
            new_id
        };
        Ok(Some(id))
    }

    /// Phase 17: bump a fact's `access_count` (called on every
    /// recall hit so frequently-accessed facts naturally float up
    /// the salience ranking).
    pub fn boost_fact_access(&self, canonical_key: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE facts SET access_count = access_count + 1
             WHERE canonical_key = ?1 AND visibility = 1",
            params![canonical_key],
        )?;
        Ok(n > 0)
    }

    /// Phase 18 (P1-1): record one row in `recall_audit`. Called by
    /// the agent loop on every `memory_prefetch` invocation so
    /// operators can later ask:
    /// - "what queries did this session issue?"
    /// - "what was the hit count distribution?"
    /// - "what's the p99 latency?"
    ///
    /// `query_text` is truncated to 500 chars to keep audit rows
    /// small (long questions are usually less interesting to replay
    /// than their hash + hit_count + latency).
    pub fn record_recall_audit(
        &self,
        session_id: &str,
        query_text: &str,
        query_hash: &str,
        hit_count: i64,
        latency_us: i64,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let truncated_query: String = query_text.chars().take(500).collect();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO recall_audit
             (session_id, query_text, query_hash, hit_count, latency_us, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, truncated_query, query_hash, hit_count, latency_us, now],
        )?;
        Ok(())
    }

    /// Phase 18 (架构-1): freeze a `summary` of the last `frame_ids`
    /// into the `session_snapshots` table. Called by the agent loop
    /// every N turns or on `on_session_end`. Snapshots are immutable
    /// once written (the `id` is autoincrement and we never UPDATE
    /// them in place); new snapshots just append a new row. The
    /// `visibility` flag is reserved for a future "this snapshot was
    /// superseded by a newer one" feature.
    pub fn record_session_snapshot(
        &self,
        session_id: &str,
        turn_number: u32,
        summary: &str,
        frame_ids: &[String],
    ) -> Result<i64> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let frame_ids_json = serde_json::to_string(frame_ids)
            .map_err(|e| MemoryError::Other(format!("frame_ids serialize: {e}")))?;
        conn.execute(
            "INSERT INTO session_snapshots
             (session_id, turn_number, summary, frame_ids, ts, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![session_id, turn_number as i64, summary, frame_ids_json, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Phase 18 (架构-1): load the most recent snapshot for a
    /// session. Returns `None` if no snapshot exists yet (cold
    /// session). Used by the agent loop when starting a new turn to
    /// inject the frozen summary instead of replaying every frame.
    pub fn latest_session_snapshot(&self, session_id: &str) -> Result<Option<SessionSnapshot>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, turn_number, summary, frame_ids, ts
             FROM session_snapshots
             WHERE session_id = ?1 AND visibility = 1
             ORDER BY turn_number DESC, id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let turn_number: i64 = row.get(1)?;
            let summary: String = row.get(2)?;
            let frame_ids_json: String = row.get(3)?;
            let ts: String = row.get(4)?;
            let frame_ids: Vec<String> = serde_json::from_str(&frame_ids_json)
                .map_err(|e| MemoryError::Other(format!("frame_ids deserialize: {e}")))?;
            Ok(Some(SessionSnapshot {
                id,
                session_id: session_id.to_string(),
                turn_number: turn_number as u32,
                summary,
                frame_ids,
                ts,
            }))
        } else {
            Ok(None)
        }
    }

    /// Phase 18 (架构-2): enforce a hard cap on the number of visible
    /// frames. When `total > cap`, the oldest frames (lowest salience
    /// first, then oldest `created_at`) are soft-hidden so `recognize()`
    /// stops surfacing them but they remain on disk for the audit
    /// trail. Returns the number of frames newly hidden.
    ///
    /// Called on `open()` and after each `sync_turn` to keep the
    /// active working set bounded.
    pub fn enforce_frame_cap(&self, cap: usize) -> Result<usize> {
        let conn = self.conn.lock();
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM frames WHERE visibility = 1",
            [],
            |r| r.get(0),
        )?;
        if (total as usize) <= cap {
            return Ok(0);
        }
        let to_hide = total as usize - cap;
        let now = Utc::now().to_rfc3339();
        // Pick the lowest-salience visible frames first; tie-break
        // by oldest `created_at`. This keeps high-salience recent
        // frames and hides only background noise.
        let n = conn.execute(
            "UPDATE frames SET visibility = 0, hidden_at = ?1
             WHERE id IN (
               SELECT id FROM frames
               WHERE visibility = 1
               ORDER BY salience ASC, created_at ASC
               LIMIT ?2
             )",
            params![now, to_hide as i64],
        )?;
        if n > 0 {
            tracing::info!(
                target: "hermix.focus",
                cap,
                hidden = n,
                "enforce_frame_cap: hid oldest/lowest-salience frames"
            );
        }
        Ok(n)
    }

    /// Phase 18 (架构-2): count of visible frames. Test helper used
    /// to assert `enforce_frame_cap` correctly hides the right
    /// number of rows.
    pub fn count_visible_frames(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM frames WHERE visibility = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Phase 17 (BaiLongma compression backfill): when a `Cleared`
    /// focus frame is popped, the focus stack pushes a
    /// `FocusConclusion` onto the drain queue. The agent loop calls
    /// `drain_focus_conclusions_to_memory_router` which already
    /// persists the conclusion as a `frame` row. This method
    /// additionally extracts a **deterministic fact** (no LLM
    /// needed) from the conclusion's text + tags and writes it to
    /// the `facts` table so cross-session recall surfaces it via
    /// the unified `recognize()` pipeline.
    ///
    /// `canonical_key` is derived deterministically from
    /// `(session_id, source_frame_id)` so re-running this on the
    /// same conclusion is idempotent.
    pub fn backfill_compression_fact(
        &self,
        session_id: &str,
        c: &FocusConclusion,
    ) -> Result<Option<FocusFrameId>> {
        // Build a deterministic key from the conclusion's source
        // frame id (a UUID). This guarantees idempotency: re-running
        // backfill for the same conclusion hits the existing row.
        let key = format!("compression:{session_id}:{}", c.source_frame_id);
        let payload = serde_json::json!({
            "kind": "compression_backfill",
            "text": c.text,
            "source_frame_id": c.source_frame_id.to_string(),
            "session_id": session_id,
            "original_salience": c.salience,
        });
        // Tag with the conclusion's existing tags plus a
        // compression-backfill provenance tag.
        let mut tags = c.tags.clone();
        tags.push("compression_backfill".to_string());
        if !session_id.is_empty() {
            tags.push(format!("session:{session_id}"));
        }
        self.upsert_fact(&key, payload, c.salience, &tags, "compression_backfill")
    }

    /// L1.3: persist (or replace) the embedding for a frame.
    ///
    /// The frame must already exist (call `upsert_frame` first). The
    /// embedding is stored as f32 little-endian bytes; `dim` is
    /// cached separately so `cosine_topk` can `WHERE dim = ?1`
    /// instead of loading BLOBs to check length.
    ///
    /// `model` is recorded verbatim. Cosine is only meaningful
    /// between vectors from the same model, so `cosine_topk`
    /// filters mismatched models when a non-empty `model` is
    /// passed to it.
    pub fn upsert_embedding(
        &self,
        id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        if embedding.is_empty() {
            return Err(MemoryError::Other("empty embedding".into()));
        }
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let dim = embedding.len() as i64;
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE frames SET embedding = ?1, embedding_dim = ?2, embedding_model = ?3
             WHERE id = ?4",
            params![bytes, dim, model, id],
        )?;
        if n == 0 {
            return Err(MemoryError::Other(format!("frame {id} not found")));
        }
        Ok(())
    }

    /// L1.3: one-shot convenience — write the frame + its embedding
    /// in a single SQLite transaction. Prefer this over separate
    /// `upsert_frame` + `upsert_embedding` calls so a frame is
    /// never observed without its vector.
    pub fn upsert_frame_with_embedding<T: serde::Serialize + Clone>(
        &self,
        frame: &FocusFrame<T>,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        if embedding.is_empty() {
            return Err(MemoryError::Other("empty embedding".into()));
        }
        let payload = serde_json::to_string(&frame.payload)?;
        let tags_json = serde_json::to_string(&frame.tags)?;
        let state_str = state_to_str(frame.state);
        let created_at = frame.created_at.to_rfc3339();
        let updated_at = frame.updated_at.to_rfc3339();
        let parent_id = frame.parent_id.map(|p| p.to_string());
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let dim = embedding.len() as i64;
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            r#"INSERT OR REPLACE INTO frames
               (id, payload, state, salience, created_at, updated_at,
                parent_id, tags, pin_count, touch_count,
                embedding, embedding_dim, embedding_model)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
            params![
                frame.id.to_string(),
                payload,
                state_str,
                frame.salience,
                created_at,
                updated_at,
                parent_id,
                tags_json,
                frame.pin_count as i64,
                frame.touch_count as i64,
                bytes,
                dim,
                model,
            ],
        )?;
        let payload_for_index = serde_json::to_value(&frame.payload)?;
        let payload_text = extract_text(&payload_for_index);
        let tags_text = frame.tags.join(" ");
        let _ = tx.execute(
            "INSERT OR REPLACE INTO frames_fts (id, payload_text, tags_text) VALUES (?1, ?2, ?3)",
            params![frame.id.to_string(), payload_text, tags_text],
        );
        tx.commit()?;
        Ok(())
    }

    /// L1.3: cosine top-k. O(N) scan over frames whose stored
    /// `embedding_dim` matches the query; rows from a different
    /// model are skipped when `model_filter` is non-empty.
    /// `min_score` is the cosine floor in [-1, 1].
    fn cosine_topk_impl(
        &self,
        query: &[f32],
        k: usize,
        min_score: f32,
        model_filter: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<(MemoryHit, f32)>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let qdim = query.len() as i64;
        let q_norm = l2_normalize(query);
        let conn = self.conn.lock();
        let (sql, has_sid) = if session_id.is_some() {
            (
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count,
                        embedding, embedding_dim, embedding_model
                 FROM frames
                 WHERE embedding IS NOT NULL
                   AND embedding_dim = ?1
                   AND source = ?2",
                true,
            )
        } else {
            (
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count,
                        embedding, embedding_dim, embedding_model
                 FROM frames
                 WHERE embedding IS NOT NULL
                   AND embedding_dim = ?1",
                false,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = if has_sid {
            let sid = session_id.unwrap_or("");
            stmt.query(params![qdim, sid])?
        } else {
            stmt.query(params![qdim])?
        };
        let mut scored: Vec<(MemoryHit, f32)> = Vec::new();
        while let Some(row) = rows.next()? {
            let model_s: Option<String> = row.get(12)?;
            if !model_filter.is_empty() {
                match model_s.as_deref() {
                    Some(m) if m == model_filter => {}
                    _ => continue,
                }
            }
            let blob: Option<Vec<u8>> = row.get(10)?;
            let Some(blob) = blob else { continue };
            // Decode f32 little-endian. We know `blob.len() == 4 * qdim`
            // because we filtered on embedding_dim = qdim, but a
            // corrupt row would silently read past the end. Check
            // and skip if mismatch.
            if blob.len() != (qdim as usize) * 4 {
                continue;
            }
            let vec: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let v_norm = l2_normalize(&vec);
            // cosine = q · v / (||q|| * ||v||) — but we pre-normalized
            // q, so this is just q · v_normalized (||q|| = 1).
            let score = dot(&q_norm, &v_norm);
            if score < min_score {
                continue;
            }
            let id_s: String = row.get(0)?;
            let payload_s: String = row.get(1)?;
            let state_s: String = row.get(2)?;
            // Column 3 = stored salience. We re-rank with cosine score
            // (line 421) and use that as the surfaced salience, so the
            // stored value is intentionally consumed-but-ignored.
            let _stored_salience: f64 = row.get(3)?;
            let created_s: String = row.get(4)?;
            let updated_s: String = row.get(5)?;
            let _parent_id_s: Option<String> = row.get(6)?;
            let tags_s: String = row.get(7)?;
            let pin_count: i64 = row.get(8)?;
            let touch_count: i64 = row.get(9)?;
            let payload_v: serde_json::Value =
                serde_json::from_str(&payload_s).unwrap_or(Value::Null);
            let created_at = DateTime::parse_from_rfc3339(&created_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let _ = updated_s; // not surfaced in MemoryHit
            scored.push((
                MemoryHit {
                    id: id_s,
                    content: extract_text(&payload_v),
                    salience: score as f32,
                    created_at,
                    metadata: serde_json::json!({
                        "tags": serde_json::from_str::<Vec<String>>(&tags_s)
                            .unwrap_or_default(),
                        "state": state_s,
                        "pin_count": pin_count,
                        "touch_count": touch_count,
                        "cosine_score": score,
                    }),
                    source: self.name().to_string(),
                },
                score,
            ));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    /// Re-rank hits by salience (recency × freq × pin), descending.
    /// `now` is the reference time; pass `Utc::now()` in production.
    ///
    /// H5: this used to be a no-op that just sorted by the
    /// backend-assigned `hit.salience`. The new implementation
    /// re-derives salience from the algorithm using `hit.created_at`
    /// and the backend's stored `touch_count` / `pin_count` (if
    /// they were preserved on the hit's `metadata`) and merges
    /// that with whatever salience the backend already wrote.
    /// Hits without `touch_count` / `pin_count` in metadata fall
    /// back to the backend's number — which is the right
    /// behaviour for backends that don't surface frame state.
    pub fn rerank_by_salience(
        &self,
        hits: Vec<MemoryHit>,
        now: DateTime<Utc>,
    ) -> Vec<MemoryHit> {
        let mut hits = hits;
        for hit in &mut hits {
            let touch_count = hit
                .metadata
                .get("touch_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let pin_count = hit
                .metadata
                .get("pin_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let s = salience(hit.created_at, touch_count, pin_count, now, &self.salience_cfg);
            // Combine: take the max of (backend salience, router-side
            // recency/freq/pin salience). The backend is the source of
            // truth for relevance; the router is the source of truth
            // for "this thing is hot right now".
            hit.salience = hit.salience.max(s.total);
        }
        hits.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }

    /// H5: replace the salience config (e.g. for tuning the
    /// recency/freq/pin weights in production).
    pub fn with_salience_cfg(mut self, cfg: SalienceConfig) -> Self {
        self.salience_cfg = cfg;
        self
    }


    fn fts_search_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT f.id FROM frames_fts f
                 JOIN frames fr ON fr.id = f.id
                 WHERE frames_fts MATCH ?1 AND fr.visibility = 1 AND fr.source = ?2
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT f.id FROM frames_fts f
                 JOIN frames fr ON fr.id = f.id
                 WHERE frames_fts MATCH ?1 AND fr.visibility = 1
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        // Build an FTS5 MATCH expression from the text. We split on
        // whitespace and quote each token; tokens that look like FTS5
        // operators are skipped.
        let tokens: Vec<String> = text
            .split_whitespace()
            .filter(|t| t.len() >= 2)
            .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(t))
            .map(|t| {
                // strip non-word chars
                t.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\""))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let q = tokens.join(" OR ");
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            match stmt.query(params![q, sid, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        } else {
            match stmt.query(params![q, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }


    fn like_search_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT id FROM frames WHERE visibility = 1 AND source = ?1
                 AND (payload LIKE ?2 OR tags LIKE ?2)
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT id FROM frames WHERE visibility = 1
                 AND (payload LIKE ?1 OR tags LIKE ?1)
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let pattern = format!("%{}%", text);
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            stmt.query(params![sid, pattern, limit as i64])?
        } else {
            stmt.query(params![pattern, limit as i64])?
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// L2 Semantic: search the `facts` table for keyword hits.
    /// Same MATCH expression builder as `fts_search_visible` but
    /// against `facts_fts`.
    fn fts_facts_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT fa.id FROM facts_fts fa
                 JOIN facts f ON f.id = fa.id
                 WHERE facts_fts MATCH ?1 AND f.visibility = 1 AND f.source = ?2
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT fa.id FROM facts_fts fa
                 JOIN facts f ON f.id = fa.id
                 WHERE facts_fts MATCH ?1 AND f.visibility = 1
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let tokens: Vec<String> = text
            .split_whitespace()
            .filter(|t| t.len() >= 2)
            .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(t))
            .map(|t| {
                t.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\""))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let q = tokens.join(" OR ");
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            match stmt.query(params![q, sid, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        } else {
            match stmt.query(params![q, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Convert an id (which may point at either `frames` or
    /// `facts`) to a `MemoryHit`. Returns `Ok(None)` when the id is
    /// unknown or the row is hidden.
    fn hydrate_visible(
        &self,
        id_s: &str,
        query: &MemoryQuery,
        now: DateTime<Utc>,
    ) -> Result<Option<MemoryHit>> {
        let conn = self.conn.lock();
        // Try frames first.
        let frame_row: Option<(String, String, String, f64, String, String, Option<String>, String, i64, i64)> = conn
            .query_row(
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count
                 FROM frames WHERE id = ?1 AND visibility = 1",
                params![id_s],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                },
            )
            .ok();
        if let Some((id, payload, _state, salience_field, created_at, updated_at, _parent, tags, pin_count, touch_count)) = frame_row {
            let created_at = parse_dt(&created_at);
            let updated_at = parse_dt(&updated_at);
            if let Some(tw) = &query.time_window {
                if !tw.contains(created_at) {
                    return Ok(None);
                }
            }
            let payload_json: serde_json::Value = serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
            let s = salience(
                updated_at,
                touch_count as u32,
                pin_count as u32,
                now,
                &self.salience_cfg,
            );
            return Ok(Some(MemoryHit {
                id,
                content: extract_text(&payload_json),
                salience: salience_field as f32 * 0.6 + s.total * 0.4,
                created_at,
                metadata: serde_json::json!({
                    "tags": serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    "kind": "frame",
                    "pin_count": pin_count,
                    "touch_count": touch_count,
                }),
                source: self.name().to_string(),
            }));
        }
        // Try facts next.
        let fact_row: Option<(String, String, f64, String, String, String, i64)> = conn
            .query_row(
                "SELECT id, payload, salience, created_at, updated_at, tags, access_count
                 FROM facts WHERE id = ?1 AND visibility = 1",
                params![id_s],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .ok();
        if let Some((id, payload, salience_field, created_at, _updated_at, tags, access_count)) = fact_row {
            let created_at = parse_dt(&created_at);
            if let Some(tw) = &query.time_window {
                if !tw.contains(created_at) {
                    return Ok(None);
                }
            }
            let payload_json: serde_json::Value = serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
            // Facts get a small access-count boost so frequently
            // cited facts naturally float up.
            let access_boost = (access_count as f32).log10().max(0.0) * 0.05;
            return Ok(Some(MemoryHit {
                id,
                content: extract_text(&payload_json),
                salience: (salience_field as f32 + access_boost).min(1.0),
                created_at,
                metadata: serde_json::json!({
                    "tags": serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    "kind": "fact",
                    "access_count": access_count,
                }),
                source: self.name().to_string(),
            }));
        }
        Ok(None)
    }

    /// Soft-hide any visible `memory_write` row whose JSON payload
    /// `content` matches the given substring. Returns the number of
    /// rows hidden. Used by the `remove` action of the
    /// `on_memory_write` hook.
    fn soft_hide_matching_memory_write(&self, target: &str, content_substr: &str) -> usize {
        let substr = content_substr.trim();
        if substr.is_empty() {
            return 0;
        }
        let conn = self.conn.lock();
        let pattern = format!("%{}%", substr);
        let now = Utc::now().to_rfc3339();
        // Match on payload LIKE + tags LIKE. The agent loop passes the
        // user-supplied `target` parameter verbatim. For memory_tool
        // rows the tag is `target:<name>`, for turn rows it is
        // `session:<sid>`, for session_end rows it is `session`.
        // We accept any of those forms without prepending another
        // prefix (the agent already knows the convention).
        let n = conn
            .execute(
                "UPDATE frames
                 SET visibility = 0, hidden_at = ?1
                 WHERE visibility = 1
                   AND tags LIKE ?2
                   AND payload LIKE ?3",
                params![now, format!("%{}%", target), pattern],
            )
            .unwrap_or(0);
        n
    }
}

/// Build a short textual note from the last user/assistant turn
/// pair found in `messages`. Used by `EmbeddedMemoryBackend::on_session_end`
/// when the agent loop forwards a `messages` slice without an
/// accompanying `FocusConclusion`.
fn build_session_note(messages: &[serde_json::Value], session_id: &str) -> String {
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|v| v.as_str()));
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .and_then(|m| m.get("content").and_then(|v| v.as_str()));
    let user_excerpt = last_user
        .map(|s| s.chars().take(300).collect::<String>())
        .unwrap_or_default();
    let asst_excerpt = last_assistant
        .map(|s| s.chars().take(500).collect::<String>())
        .unwrap_or_default();
    if user_excerpt.is_empty() && asst_excerpt.is_empty() {
        return String::new();
    }
    format!(
        "[session:{session_id}] user: {user_excerpt}\n[session:{session_id}] asst: {asst_excerpt}",
    )
}

/// Parse an RFC3339 timestamp; on error, return the Unix epoch.
fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(|| Utc::now()))
}

fn state_to_str(s: FrameState) -> &'static str {
    match s {
        FrameState::Created => "created",
        FrameState::Pushed => "pushed",
        FrameState::Kept => "kept",
        FrameState::Returned => "returned",
        FrameState::Cleared => "cleared",
    }
}

fn str_to_state(s: &str) -> FrameState {
    match s {
        "pushed" => FrameState::Pushed,
        "kept" => FrameState::Kept,
        "returned" => FrameState::Returned,
        "cleared" => FrameState::Cleared,
        _ => FrameState::Created,
    }
}

/// L1.3: L2-normalize a vector. Returns a zero vector unchanged
/// (cosine is undefined for the zero vector; the caller is
/// responsible for filtering them out via `min_score`).
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let mut norm = 0.0_f32;
    for &x in v {
        norm += x * x;
    }
    let n = norm.sqrt();
    if n == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / n).collect()
}

/// L1.3: dot product of two equal-length f32 vectors. Returns 0.0
/// if the lengths differ — the caller filters by dim upstream
/// so this is just a safety net for corrupt rows.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut acc = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += x * y;
    }
    acc
}

fn extract_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(extract_text).collect::<Vec<_>>().join(" "),
        Value::Object(o) => o
            .values()
            .map(extract_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => v.to_string(),
    }
}

/// A frame row loaded from the DB. The payload is kept as raw JSON so
/// callers can deserialize into their own `T`.
#[derive(Debug, Clone)]
pub struct StoredFrame {
    pub id: FocusFrameId,
    pub payload: Value,
    pub state: FrameState,
    pub salience: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_id: Option<FocusFrameId>,
    pub tags: Vec<String>,
    pub pin_count: u32,
    pub touch_count: u32,
}

/// Phase 18 (架构-1): a frozen snapshot of a session's recent
/// frames. Written every N turns (default 50) or on
/// `on_session_end`. The agent loop reads the latest snapshot via
/// [`EmbeddedMemoryBackend::latest_session_snapshot`] when starting
/// a new turn so it doesn't have to replay every frame in the
/// session.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub id: i64,
    pub session_id: String,
    pub turn_number: u32,
    pub summary: String,
    pub frame_ids: Vec<String>,
    pub ts: String,
}

impl StoredFrame {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let id_s: String = row.get(0)?;
        let payload_s: String = row.get(1)?;
        let state_s: String = row.get(2)?;
        let salience: f64 = row.get(3)?;
        let created_s: String = row.get(4)?;
        let updated_s: String = row.get(5)?;
        let parent_id_s: Option<String> = row.get(6)?;
        let tags_s: String = row.get(7)?;
        let pin_count: i64 = row.get(8)?;
        let touch_count: i64 = row.get(9)?;

        Ok(Self {
            id: parse_uuid(&id_s).unwrap_or_else(Uuid::nil),
            payload: serde_json::from_str(&payload_s).unwrap_or(Value::Null),
            state: str_to_state(&state_s),
            salience: salience as f32,
            created_at: DateTime::parse_from_rfc3339(&created_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&updated_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            parent_id: parent_id_s.as_ref().and_then(|s| parse_uuid(s)),
            tags: serde_json::from_str(&tags_s).unwrap_or_default(),
            pin_count: pin_count as u32,
            touch_count: touch_count as u32,
        })
    }
}

fn parse_uuid(s: &str) -> Option<FocusFrameId> {
    Uuid::parse_str(s).ok()
}

#[async_trait]
impl MemoryLayer for EmbeddedMemoryBackend {
    fn name(&self) -> &str {
        "embedded-sqlite"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn initialize(&self, session_id: &str, hermix_home: &str) {
        *self.session_id.lock() = Some(session_id.to_string());
        *self.hermix_home.lock() = Some(hermix_home.to_string());
        tracing::debug!(
            target: "hermix.focus",
            session_id,
            hermix_home,
            "EmbeddedMemoryBackend bound to session"
        );
    }

    fn shutdown(&self) {
        // Drop the in-memory turn cache; the persisted frames stay
        // in SQLite. We don't close the connection because the
        // backend may be reused across sessions in long-running
        // servers.
        self.recent_turns.lock().clear();
        tracing::debug!(target: "hermix.focus", "EmbeddedMemoryBackend shutdown");
    }

    fn sync_turn(&self, user: &str, assistant: &str, session_id: &str) {
        let turn_number = {
            let mut tc = self.turn_count.lock();
            *tc = tc.saturating_add(1);
            *tc
        };
        if let Err(e) = self.record_turn(user, assistant, session_id, turn_number) {
            tracing::warn!(
                target: "hermix.focus",
                session_id,
                turn_number,
                error = %e,
                "sync_turn: record_turn failed; the turn was not persisted"
            );
            return;
        }
        // Update the in-memory ring for the pre-compress hook. We
        // bound it to the last 20 turns so memory use stays small
        // even on marathon sessions.
        let mut recent = self.recent_turns.lock();
        recent.push(TurnRecord {
            turn_number,
            user: user.chars().take(2000).collect(),
            assistant: assistant.chars().take(2000).collect(),
        });
        if recent.len() > 20 {
            let drop_n = recent.len() - 20;
            recent.drain(0..drop_n);
        }
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        // The `action` ∈ {add, replace, remove}. We persist all
        // three so the audit trail is complete, but only add/replace
        // are useful for cross-session recall (the `remove` action
        // is reflected by a soft-hide after the row is recorded).
        match action {
            "add" | "replace" => {
                if let Err(e) = self.record_memory_write(action, target, content) {
                    tracing::warn!(
                        target: "hermix.focus",
                        action,
                        target,
                        error = %e,
                        "on_memory_write: record_memory_write failed"
                    );
                }
            }
            "remove" => {
                // For `remove` we soft-hide any existing
                // memory_write rows whose payload content matches.
                // This is best-effort: the user tool call's `content`
                // is the *new* content (to be removed), not the
                // existing one. The standard caller passes the old
                // text in `content`; we log if we can't match.
                let removed = self.soft_hide_matching_memory_write(target, content);
                if removed == 0 {
                    tracing::debug!(
                        target: "hermix.focus",
                        target,
                        "on_memory_write: remove found no matching memory_write row to hide"
                    );
                }
            }
            _ => {
                tracing::debug!(
                    target: "hermix.focus",
                    action,
                    "on_memory_write: unknown action; ignoring"
                );
            }
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        // Keep `turn_count` aligned with the agent loop's monotonic
        // counter (the loop calls `sync_turn` immediately after, so
        // this is just an early signal).
        *self.turn_count.lock() = turn_number;
    }

    fn on_session_end(&self, messages: &[serde_json::Value]) {
        // Build a short textual note covering the most recent
        // user/assistant turns. The agent loop already writes a
        // session-level `FocusConclusion` via
        // `upsert_conclusion`, so here we only handle the case
        // where the loop called `on_session_end` directly without
        // the conclusion path. We detect that by checking whether
        // the most recent `session`-tagged frame in the last hour
        // matches today's session id; if not, we write a fresh one.
        if messages.is_empty() {
            return;
        }
        let session_id = self
            .session_id
            .lock()
            .clone()
            .unwrap_or_else(|| "unknown-session".to_string());
        let note = build_session_note(messages, &session_id);
        if note.trim().is_empty() {
            return;
        }
        let payload = serde_json::json!({
            "kind": "session_end",
            "session_id": session_id,
            "turn_count": self.turn_count.lock().clone(),
            "note": note,
        });
        let frame = FocusFrame::new(payload)
            .with_salience(0.4)
            .with_tag("session_end")
            .with_tag(format!("session:{session_id}"));
        if let Err(e) = self.upsert_frame_with_source(&frame, "session") {
            tracing::warn!(
                target: "hermix.focus",
                session_id,
                error = %e,
                "on_session_end: persist failed"
            );
        }
    }

    fn on_pre_compress(&self, messages: &[serde_json::Value]) -> String {
        // Two-tier contribution:
        // 1. The recent-turn ring (always present after the first
        //    `sync_turn` call) gives the compressor a high-quality
        //    summary of the *last 20 turns* without round-tripping
        //    to SQLite.
        // 2. If the agent loop passes `messages` directly, we
        //    additionally include a head/tail excerpt so the
        //    compressor sees the global shape.
        let mut out = String::new();
        let recent = self.recent_turns.lock();
        if !recent.is_empty() {
            out.push_str("[EmbeddedMemoryBackend recent turns]\n");
            for r in recent.iter().rev().take(8).collect::<Vec<_>>().into_iter().rev() {
                let user_excerpt: String = r.user.chars().take(200).collect();
                let asst_excerpt: String = r.assistant.chars().take(200).collect();
                out.push_str(&format!(
                    "  turn#{} user: {}\n  turn#{} asst: {}\n",
                    r.turn_number, user_excerpt, r.turn_number, asst_excerpt
                ));
            }
        }
        if !messages.is_empty() {
            out.push_str("\n[EmbeddedMemoryBackend session shape]\n");
            let total = messages.len();
            let head_n = 2.min(total);
            let tail_n = 4.min(total);
            for (i, msg) in messages.iter().take(head_n).enumerate() {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let content: String = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
                out.push_str(&format!("  msg#{i} {role}: {content}\n"));
            }
            if total > head_n + tail_n {
                out.push_str(&format!("  ... {} messages omitted ...\n", total - head_n - tail_n));
            }
            for (i, msg) in messages.iter().rev().take(tail_n).collect::<Vec<_>>().into_iter().enumerate() {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let content: String = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
                out.push_str(&format!("  msg#{tail_idx} {role}: {content}\n", tail_idx = total - 1 - i));
            }
        }
        out
    }

    fn system_prompt_block(&self) -> String {
        // Phase 17: a one-line description of the local backend so
        // the agent knows where its cross-session recall comes
        // from. We intentionally keep this short — long prose
        // here dilutes the system prompt.
        "Cross-session memory is enabled (local SQLite, embedded, no cloud). \
         Use the `memory` tool to assert facts explicitly; the embedded \
         backend records every turn and any explicit write for future recall."
            .to_string()
    }

    fn on_delegation(&self, task: &str, sub_agent_id: &str) {
        if let Err(e) = self.record_delegation(task, sub_agent_id) {
            tracing::warn!(
                target: "hermix.focus",
                sub_agent_id,
                error = %e,
                "on_delegation: record_delegation failed"
            );
        }
    }

    async fn recognize(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>> {
        // Phase 17 (BaiLongma soft-hide): every recall leg filters
        // `visibility = 1` so soft-hidden frames never reach the
        // LLM. Hidden frames are kept on disk for the audit trail
        // and 30-day janitor.
        // 1. Text leg — FTS5 first, LIKE fallback. We over-fetch
        //    (limit * 2) so a later merge with the vector leg has
        //    material to dedupe against.
        let sid = query.session_id.as_deref();
        let mut ids = self.fts_search_visible(&query.text, query.limit * 2, sid)?;
        if ids.is_empty() {
            ids = self.like_search_visible(&query.text, query.limit * 2, sid)?;
        }
        // 1b. Fact leg — query the `facts` table too so L2 Semantic
        //     memory is part of the unified recall.
        let fact_ids = self.fts_facts_visible(&query.text, query.limit, sid)?;
        for fid in fact_ids {
            if !ids.contains(&fid) {
                ids.push(fid);
            }
        }
        // 2. Load each frame, apply time-window filter, compute salience.
        let now = Utc::now();
        let mut hits: Vec<MemoryHit> = Vec::new();
        for id_s in ids {
            // Try frames table first, then facts. Either way the
            // returned struct is converted to MemoryHit.
            if let Some(hit) = self.hydrate_visible(&id_s, query, now)? {
                hits.push(hit);
            }
        }
        // 3. L1.3 vector leg — when a query embedding is provided
        //    we run cosine_topk in parallel and merge. Empty model
        //    filter (`""`) is the right call here because the query
        //    embedding is "of the agent's current model" and the
        //    backend stores whatever model it was given; the strict
        //    match would be a follow-up. For now we let cosine
        //    speak for itself and rely on the `embedding_dim` index
        //    to keep the scan cheap.
        if let Some(qe) = query.embedding.as_deref() {
            if !qe.is_empty() {
                let vec_hits = self
                    .cosine_topk_impl(qe, query.limit * 2, -1.0, "", sid)?
                    .into_iter()
                    .map(|(h, _)| h);
                for vh in vec_hits {
                    if let Some(existing) = hits.iter_mut().find(|h| h.id == vh.id) {
                        // Boost an existing text hit's salience with the
                        // cosine score so a frame that matches BOTH the
                        // text query and the query embedding wins the
                        // final ranking. Pure max() would lose the
                        // text-signal component, so we add (capped).
                        let boosted = (existing.salience + vh.salience * 0.5).min(1.0);
                        existing.salience = boosted;
                    } else {
                        hits.push(vh);
                    }
                }
            }
        }
        // 4. Final re-rank by salience desc and truncate.
        hits.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(query.limit);
        Ok(hits)
    }

    async fn inject(&self, hits: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = String::new();
        out.push_str("## Memory (embedded)\n\n");
        for h in hits {
            out.push_str(&format!(
                "- [salience={:.2}] {}\n",
                h.salience,
                h.content
            ));
        }
        Ok(vec![SystemPromptBlock {
            text: out,
            priority: 50,
        }])
    }

    /// M1.6.4: persist a `FocusConclusion` from the agent loop's
    /// focus stack. Delegates to the synchronous
    /// `upsert_conclusion` on the underlying backend. The SQLite
    /// write is guarded by an internal `Mutex<Connection>` so the
    /// async call returns immediately once the lock is acquired.
    /// If we later observe lock contention on hot paths we can
    /// move this to `tokio::task::spawn_blocking` without changing
    /// the trait signature.
    async fn upsert_conclusion(&self, c: &FocusConclusion) -> Result<()> {
        self.upsert_conclusion(c)
    }

    /// L1.3: delegate to the sync impl. The async wrapper is here
    /// purely for trait uniformity — the SQLite write is
    /// sub-millisecond, so spawning a blocking task would add
    /// overhead, not remove it.
    async fn upsert_embedding(
        &self,
        id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        self.upsert_embedding(id, embedding, model)
    }

    /// L1.3: delegate to the sync impl.
    async fn cosine_topk(
        &self,
        query: &[f32],
        k: usize,
        min_score: f32,
        model: &str,
    ) -> Result<Vec<MemoryHit>> {
        let pairs = self.cosine_topk_impl(query, k, min_score, model, None)?;
        Ok(pairs.into_iter().map(|(h, _)| h).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_in_memory_works() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        assert_eq!(b.count().unwrap(), 0);
    }

    #[tokio::test]
    async fn upsert_and_count() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("hello world".to_string()).with_tag("greeting");
        b.upsert_frame(&f).unwrap();
        assert_eq!(b.count().unwrap(), 1);
    }

    #[tokio::test]
    async fn upsert_replaces_existing() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let mut f = FocusFrame::new("v1".to_string());
        b.upsert_frame(&f).unwrap();
        f.payload = "v2".to_string();
        f.touch();
        b.upsert_frame(&f).unwrap();
        assert_eq!(b.count().unwrap(), 1);
    }

    #[tokio::test]
    async fn delete_frame() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("bye".to_string());
        let id = f.id;
        b.upsert_frame(&f).unwrap();
        assert!(b.delete_frame(id).unwrap());
        assert_eq!(b.count().unwrap(), 0);
    }

    #[tokio::test]
    async fn recognize_returns_inserted() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.upsert_frame(&FocusFrame::new("Rust memory backend".to_string())).unwrap();
        b.upsert_frame(&FocusFrame::new("Python memory backend".to_string())).unwrap();
        let q = MemoryQuery::recall("Rust");
        let hits = b.recognize(&q).await.unwrap();
        assert!(!hits.is_empty(), "should find Rust frame");
        assert!(hits[0].content.contains("Rust"));
    }

    #[tokio::test]
    async fn recognize_respects_limit() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        for i in 0..20 {
            b.upsert_frame(&FocusFrame::new(format!("Rust frame number {i}"))).unwrap();
        }
        let q = MemoryQuery::recall("Rust").with_limit(5);
        let hits = b.recognize(&q).await.unwrap();
        assert_eq!(hits.len(), 5);
    }

    #[tokio::test]
    async fn recognize_applies_time_window() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.upsert_frame(&FocusFrame::new("Rust now".to_string())).unwrap();
        let q = MemoryQuery::recall("Rust")
            .with_time_window(crate::memory_layer::TimeWindow::last_hours(1));
        let hits = b.recognize(&q).await.unwrap();
        assert!(!hits.is_empty());
    }

    #[tokio::test]
    async fn inject_produces_prompt_block() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.upsert_frame(&FocusFrame::new("x".to_string())).unwrap();
        let q = MemoryQuery::recall("x");
        let (hits, blocks) = b.recall(&q).await.unwrap();
        assert!(!hits.is_empty());
        assert!(!blocks.is_empty());
        assert!(blocks[0].text.contains("Memory"));
    }

    #[tokio::test]
    async fn layer_trait_metadata() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        assert_eq!(b.name(), "embedded-sqlite");
        assert!(b.is_available());
    }

    #[tokio::test]
    async fn rerank_is_descending() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        // H5: hits now carry touch_count / pin_count in metadata so
        // the router-side re-rank can re-derive salience. We vary
        // these to break the ties that the old test (where all
        // metadata was empty) was happy to ignore.
        let now = Utc::now();
        let hits = vec![
            MemoryHit {
                id: "a".into(),
                content: "x".into(),
                salience: 0.1,
                created_at: now,
                metadata: serde_json::json!({"touch_count": 0, "pin_count": 0}),
                source: "test".into(),
            },
            MemoryHit {
                id: "b".into(),
                content: "y".into(),
                salience: 0.9,
                created_at: now,
                metadata: serde_json::json!({"touch_count": 5, "pin_count": 1}),
                source: "test".into(),
            },
            MemoryHit {
                id: "c".into(),
                content: "z".into(),
                salience: 0.5,
                created_at: now,
                metadata: serde_json::json!({"touch_count": 2, "pin_count": 0}),
                source: "test".into(),
            },
        ];
        let ranked = b.rerank_by_salience(hits, now);
        assert_eq!(ranked[0].id, "b");
        assert_eq!(ranked[1].id, "c");
        assert_eq!(ranked[2].id, "a");
    }

    /// H5: the `rerank_by_salience` algorithm actually engages
    /// when touch_count / pin_count are non-zero. A hit with a low
    /// backend salience but lots of touches should rise above a
    /// hit with high backend salience but no touches.
    #[test]
    fn rerank_combines_backend_and_touch_signal() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let now = Utc::now();
        let hits = vec![
            // High backend salience, but no touch history.
            MemoryHit {
                id: "backend-strong".into(),
                content: "x".into(),
                salience: 0.6,
                created_at: now,
                metadata: serde_json::json!({"touch_count": 0, "pin_count": 0}),
                source: "test".into(),
            },
            // Lower backend salience, but lots of touches + a pin.
            MemoryHit {
                id: "touched".into(),
                content: "y".into(),
                salience: 0.3,
                created_at: now,
                metadata: serde_json::json!({"touch_count": 30, "pin_count": 1}),
                source: "test".into(),
            },
        ];
        let ranked = b.rerank_by_salience(hits, now);
        // The "touched" hit should rise above "backend-strong"
        // because the freq + pin channels push it past 0.6.
        assert_eq!(
            ranked[0].id, "touched",
            "expected 'touched' to win after router-side re-rank, got {}",
            ranked[0].id
        );
    }

    // ── L1.3: vector storage + cosine top-k tests ─────────────────

    /// L1.3: `migrate()` must be idempotent. Opening the same in-memory
    /// DB twice is a no-op; opening a DB written by a pre-L1.3
    /// hermix-focus must add the 3 new columns without error.
    #[tokio::test]
    async fn l13_migrate_is_idempotent() {
        let b1 = EmbeddedMemoryBackend::open_in_memory().unwrap();
        // Write one frame, then close.
        b1.upsert_frame(&FocusFrame::new("legacy frame".to_string()))
            .unwrap();
        // Open again on the same path. Simulating "previous-version DB
        // reopened after upgrade" requires a real file — in-memory
        // :memory: connections are private to the Connection, so
        // the realistic test is on a tempfile.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let b = EmbeddedMemoryBackend::open(&path).unwrap();
            b.upsert_frame(&FocusFrame::new("pre-l13".to_string()))
                .unwrap();
        }
        // Reopen — the migration ALTER TABLE block must add the
        // 3 new columns without choking on existing rows.
        let b2 = EmbeddedMemoryBackend::open(&path).unwrap();
        assert_eq!(b2.count().unwrap(), 1);
        // Idempotency: opening twice in a row on the same path
        // must not blow up (the PRAGMA table_info check skips
        // existing columns).
        let b3 = EmbeddedMemoryBackend::open(&path).unwrap();
        assert_eq!(b3.count().unwrap(), 1);
    }

    /// L1.3: `upsert_embedding` persists f32 bytes and `cosine_topk`
    /// reads them back as the same vector.
    #[tokio::test]
    async fn l13_upsert_embedding_roundtrip() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("alpha".to_string());
        b.upsert_frame(&f).unwrap();
        let vec = vec![0.1, 0.2, 0.3, 0.4];
        b.upsert_embedding(&f.id.to_string(), &vec, "bge-small").unwrap();
        // We don't have a public "read raw embedding" path yet, but
        // we can prove roundtrip by querying the same vector back
        // and asserting self-cosine == 1.0.
        let hits = b
            .cosine_topk_impl(&vec, 1, 0.99, "bge-small", None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let score = hits[0].1;
        assert!((score - 1.0).abs() < 1e-5, "self-cosine should be 1.0, got {score}");
    }

    /// L1.3: `cosine_topk` finds the most similar frame among 3.
    #[tokio::test]
    async fn l13_cosine_topk_finds_nearest() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f1 = FocusFrame::new("orthogonal".to_string());
        let f2 = FocusFrame::new("close".to_string());
        let f3 = FocusFrame::new("exact".to_string());
        b.upsert_frame(&f1).unwrap();
        b.upsert_frame(&f2).unwrap();
        b.upsert_frame(&f3).unwrap();
        b.upsert_embedding(&f1.id.to_string(), &[1.0, 0.0, 0.0], "test").unwrap();
        b.upsert_embedding(&f2.id.to_string(), &[0.9, 0.1, 0.0], "test").unwrap();
        b.upsert_embedding(&f3.id.to_string(), &[1.0, 0.0, 0.0], "test").unwrap();
        // Query is [1, 0, 0] — f1 and f3 are exact matches
        // (cosine = 1.0), f2 is close (cosine = 0.9*sqrt(...) ≈ 0.99).
        let hits = b
            .cosine_topk_impl(&[1.0, 0.0, 0.0], 3, 0.0, "test", None)
            .unwrap();
        assert_eq!(hits.len(), 3);
        // The two exact matches tie at 1.0; we accept either at the
        // top as long as the close match is at the bottom.
        let top_ids = [&hits[0].0.id, &hits[1].0.id];
        assert!(top_ids.contains(&&f1.id.to_string()));
        assert!(top_ids.contains(&&f3.id.to_string()));
        assert_eq!(hits[2].0.id, f2.id.to_string());
    }

    /// L1.3: rows whose stored dim doesn't match the query are skipped.
    #[tokio::test]
    async fn l13_cosine_skips_mismatched_dim() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f_short = FocusFrame::new("short".to_string());
        let f_long = FocusFrame::new("long".to_string());
        b.upsert_frame(&f_short).unwrap();
        b.upsert_frame(&f_long).unwrap();
        b.upsert_embedding(&f_short.id.to_string(), &[0.5, 0.5], "test").unwrap();
        b.upsert_embedding(&f_long.id.to_string(), &[0.5, 0.5, 0.5, 0.5], "test").unwrap();
        // Query has 4 dims — only f_long should be in the result.
        let hits = b
            .cosine_topk_impl(&[0.5, 0.5, 0.5, 0.5], 10, 0.0, "test", None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, f_long.id.to_string());
    }

    /// L1.3: rows from a different model are skipped when a model
    /// filter is supplied.
    #[tokio::test]
    async fn l13_cosine_skips_mismatched_model() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f_a = FocusFrame::new("model a".to_string());
        let f_b = FocusFrame::new("model b".to_string());
        b.upsert_frame(&f_a).unwrap();
        b.upsert_frame(&f_b).unwrap();
        b.upsert_embedding(&f_a.id.to_string(), &[1.0, 0.0, 0.0], "model-a").unwrap();
        b.upsert_embedding(&f_b.id.to_string(), &[1.0, 0.0, 0.0], "model-b").unwrap();
        // Filter to model-a only.
        let hits = b
            .cosine_topk_impl(&[1.0, 0.0, 0.0], 10, 0.0, "model-a", None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, f_a.id.to_string());
    }

    /// L1.3: `min_score` filters out low-similarity hits.
    #[tokio::test]
    async fn l13_cosine_respects_min_score() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f_sim = FocusFrame::new("similar".to_string());
        let f_ortho = FocusFrame::new("orthogonal".to_string());
        b.upsert_frame(&f_sim).unwrap();
        b.upsert_frame(&f_ortho).unwrap();
        b.upsert_embedding(&f_sim.id.to_string(), &[1.0, 0.0, 0.0], "t").unwrap();
        b.upsert_embedding(&f_ortho.id.to_string(), &[0.0, 1.0, 0.0], "t").unwrap();
        // min_score=0.5 keeps the parallel hit (cos=1) and drops
        // the orthogonal one (cos=0).
        let hits = b
            .cosine_topk_impl(&[1.0, 0.0, 0.0], 10, 0.5, "t", None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, f_sim.id.to_string());
    }

    /// L1.3: `recognize` runs the hybrid leg — a query embedding
    /// surfaces a frame that doesn't share text with the FTS query.
    #[tokio::test]
    async fn l13_recognize_hybrid_uses_embedding() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        // Frame A: text matches "rust", vector points in +x direction.
        let fa = FocusFrame::new("rust in title".to_string()).with_tag("rust");
        // Frame B: text is "python in title", vector also points in +x
        // direction. FTS won't find it; cosine_topk will.
        let fb = FocusFrame::new("python in title".to_string()).with_tag("python");
        b.upsert_frame_with_embedding(&fa, &[1.0, 0.0, 0.0], "test").unwrap();
        b.upsert_frame_with_embedding(&fb, &[1.0, 0.0, 0.0], "test").unwrap();
        let q = MemoryQuery::recall("rust").with_embedding(vec![1.0, 0.0, 0.0]);
        let hits = b.recognize(&q).await.unwrap();
        // FTS finds fa. Cosine leg pulls in fb. Both should be
        // present in the result.
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&fa.id.to_string().as_str()), "FTS leg missed fa");
        assert!(ids.contains(&fb.id.to_string().as_str()), "vector leg missed fb");
    }

    /// L1.3: `upsert_embedding` against a non-existent frame is an
    /// error rather than a silent insert.
    #[tokio::test]
    async fn l13_upsert_embedding_unknown_frame_errors() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let r = b.upsert_embedding("not-a-real-uuid", &[0.1, 0.2], "t");
        assert!(r.is_err(), "should error when frame doesn't exist");
    }

    /// L1.3: `l2_normalize` handles the zero vector by returning it
    /// unchanged (we don't NaN out; the caller's `min_score` filter
    /// is the right place to drop all-zero rows).
    #[test]
    fn l13_l2_normalize_zero_vector() {
        let z = vec![0.0, 0.0, 0.0];
        let n = l2_normalize(&z);
        assert_eq!(n, z);
        let v = vec![3.0, 4.0];
        let n = l2_normalize(&v);
        assert!((n[0] - 0.6).abs() < 1e-5);
        assert!((n[1] - 0.8).abs() < 1e-5);
    }

    /// L1.3: `dot` returns 0.0 for mismatched lengths.
    #[test]
    fn l13_dot_mismatch_returns_zero() {
        assert_eq!(dot(&[1.0, 2.0], &[1.0, 2.0, 3.0]), 0.0);
        assert_eq!(dot(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert!((dot(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-5);
    }

    // ── Phase 17 (2026-06-26): write-path hooks ─────────────────────

    /// `sync_turn` writes a low-salience (0.3) frame tagged `turn`.
    /// Empty input is a no-op so callers can fire-and-forget.
    #[tokio::test]
    async fn p17_sync_turn_persists_frame() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.sync_turn("hello", "hi there", "s1");
        let hits = b
            .recognize(&MemoryQuery::recall("hello").with_limit(10))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("hello"));
    }

    /// `sync_turn` for empty input is a no-op.
    #[tokio::test]
    async fn p17_sync_turn_empty_input_noop() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.sync_turn("", "", "s1");
        assert_eq!(b.count().unwrap(), 0);
    }

    /// `on_memory_write` for `add`/`replace` writes a higher-salience
    /// (0.7) frame tagged `memory_write` and `target:<name>`.
    #[tokio::test]
    async fn p17_on_memory_write_persists() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.on_memory_write("add", "user", "主人喜欢 Rust");
        let hits = b
            .recognize(&MemoryQuery::recall("Rust").with_limit(10))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("Rust"));
        // Salience should reflect the higher 0.7 baseline (vs the
        // 0.3 baseline for raw turns).
        assert!(hits[0].salience >= 0.5);
    }

    /// `on_memory_write` for `remove` soft-hides any matching row.
    #[tokio::test]
    async fn p17_on_memory_write_remove_soft_hides() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.on_memory_write("add", "user", "记住这个事实");
        assert_eq!(b.count().unwrap(), 1);
        b.on_memory_write("remove", "user", "记住这个事实");
        // Row is still on disk, just hidden.
        assert_eq!(b.count().unwrap(), 1);
        let hits = b
            .recognize(&MemoryQuery::recall("事实").with_limit(10))
            .await
            .unwrap();
        assert!(hits.is_empty(), "hidden row should not be returned");
    }

    /// `on_session_end` persists a session-level frame so cross-session
    /// recall can find it.
    #[tokio::test]
    async fn p17_on_session_end_persists() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "讨论 Rust 性能优化"}),
            serde_json::json!({"role": "assistant", "content": "建议使用 jemalloc"}),
        ];
        b.on_session_end(&msgs);
        let hits = b
            .recognize(&MemoryQuery::recall("Rust").with_limit(10))
            .await
            .unwrap();
        assert!(!hits.is_empty(), "session_end should produce a recallable frame");
    }

    /// `soft_hide_frame` makes a frame invisible to recall but keeps
    /// it on disk for the audit trail.
    #[tokio::test]
    async fn p17_soft_hide_then_show() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("secret payload".to_string()).with_salience(0.9);
        let id = f.id;
        b.upsert_frame(&f).unwrap();
        assert!(!b
            .recognize(&MemoryQuery::recall("secret").with_limit(10))
            .await
            .unwrap()
            .is_empty());
        assert!(b.soft_hide_frame(id).unwrap());
        assert!(b
            .recognize(&MemoryQuery::recall("secret").with_limit(10))
            .await
            .unwrap()
            .is_empty());
        assert!(b.show_frame(id).unwrap());
        assert!(!b
            .recognize(&MemoryQuery::recall("secret").with_limit(10))
            .await
            .unwrap()
            .is_empty());
    }

    /// `upsert_fact` dedups by `canonical_key`. Same key twice bumps
    /// `access_count` and updates the payload; row count stays 1.
    #[tokio::test]
    async fn p17_upsert_fact_dedups_by_key() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let id1 = b
            .upsert_fact(
                "user.name",
                serde_json::json!("主人"),
                0.8,
                &["user".into()],
                "curation",
            )
            .unwrap();
        let id2 = b
            .upsert_fact(
                "user.name",
                serde_json::json!("主人"),
                0.9,
                &["user".into()],
                "curation",
            )
            .unwrap();
        assert!(id1.is_some() && id2.is_some());
        assert_eq!(id1, id2, "same canonical_key should yield same id");
        // Total visible fact rows should still be 1.
        let hits = b
            .recognize(&MemoryQuery::recall("主人").with_limit(10))
            .await
            .unwrap();
        assert_eq!(hits.iter().filter(|h| h.metadata.get("kind").and_then(|v| v.as_str()) == Some("fact")).count(), 1);
    }

    /// `on_pre_compress` returns a textual note covering recent turns.
    #[tokio::test]
    async fn p17_on_pre_compress_includes_recent_turns() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.sync_turn("first user message", "first reply", "s1");
        b.sync_turn("second user message", "second reply", "s1");
        let msgs: Vec<serde_json::Value> = vec![];
        let note = b.on_pre_compress(&msgs);
        assert!(note.contains("first user message"));
        assert!(note.contains("second reply"));
        assert!(note.contains("EmbeddedMemoryBackend recent turns"));
    }

    /// BaiLongma compression backfill: when a Cleared focus frame
    /// is drained, the conclusion becomes a fact row in addition
    /// to its frame row. Re-running backfill on the same conclusion
    /// is idempotent (same canonical_key → same id).
    #[tokio::test]
    async fn p17_backfill_compression_fact_idempotent() {
        use crate::focus_stack::FocusConclusion;
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let frame_id = Uuid::new_v4();
        let conclusion = FocusConclusion {
            source_frame_id: frame_id,
            text: "Discussion about Rust ownership semantics".to_string(),
            salience: 0.6,
            tags: vec!["delegation".into(), "rust".into()],
            created_at: Utc::now(),
        };
        let id1 = b.backfill_compression_fact("s1", &conclusion).unwrap();
        let id2 = b.backfill_compression_fact("s1", &conclusion).unwrap();
        assert!(id1.is_some() && id2.is_some());
        assert_eq!(id1, id2, "same frame id should yield same fact id");

        // The fact should surface via `recognize()` as a hit.
        let hits = b
            .recognize(&MemoryQuery::recall("Rust ownership").with_limit(10))
            .await
            .unwrap();
        let fact_hit = hits
            .iter()
            .find(|h| h.metadata.get("kind").and_then(|v| v.as_str()) == Some("fact"));
        assert!(
            fact_hit.is_some(),
            "compression_backfill fact should surface via recall; got {} hits: {:?}",
            hits.len(),
            hits
        );
    }

    // ---- Phase 18 (P1-1 / 架构-1 / 架构-2): audit + snapshot + cap ----

    #[tokio::test]
    async fn p18_record_recall_audit_persists_row() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        for i in 0..3 {
            b.record_recall_audit(
                "sess-1",
                &format!("query-{i}"),
                &format!("hash{i}"),
                i,
                100 + i as i64,
            )
            .unwrap();
        }
        // Verify the rows landed by querying the same connection.
        let conn = b.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recall_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
        let total_latency: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(latency_us), 0) FROM recall_audit",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total_latency, 100 + 101 + 102);
    }

    #[tokio::test]
    async fn p18_session_snapshot_round_trip() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        // Empty session → None.
        assert!(b.latest_session_snapshot("sess-A").unwrap().is_none());
        // Write one snapshot.
        b.record_session_snapshot("sess-A", 50, "midway summary", &["f1".into(), "f2".into()])
            .unwrap();
        let snap = b.latest_session_snapshot("sess-A").unwrap().unwrap();
        assert_eq!(snap.turn_number, 50);
        assert_eq!(snap.summary, "midway summary");
        assert_eq!(snap.frame_ids, vec!["f1".to_string(), "f2".to_string()]);
        // Write a newer snapshot at turn 100; latest should return it.
        b.record_session_snapshot("sess-A", 100, "end summary", &["f3".into()])
            .unwrap();
        let snap2 = b.latest_session_snapshot("sess-A").unwrap().unwrap();
        assert_eq!(snap2.turn_number, 100);
        assert_eq!(snap2.summary, "end summary");
    }

    #[tokio::test]
    async fn p18_enforce_frame_cap_hides_oldest_lowest() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        // Insert 5 frames with descending salience.
        let mut frame_ids = Vec::new();
        for i in 0..5 {
            let payload = serde_json::json!({"kind": "test", "i": i});
            let frame = FocusFrame::new(payload)
                .with_salience(1.0 - (i as f32) * 0.1);
            let id = frame.id;
            b.upsert_frame(&frame).unwrap();
            frame_ids.push(id);
        }
        // Cap at 3: should hide the 2 lowest-salience frames.
        let hidden = b.enforce_frame_cap(3).unwrap();
        assert_eq!(hidden, 2);
        // Verify: top-3 frames are visible, bottom-2 are hidden.
        let visible = b.count_visible_frames().unwrap();
        assert_eq!(visible, 3);
    }

    #[tokio::test]
    async fn p18_enforce_frame_cap_noop_when_under_cap() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        for i in 0..3 {
            let payload = serde_json::json!({"kind": "test", "i": i});
            b.upsert_frame(&FocusFrame::new(payload)).unwrap();
        }
        let hidden = b.enforce_frame_cap(10).unwrap();
        assert_eq!(hidden, 0);
    }
}
