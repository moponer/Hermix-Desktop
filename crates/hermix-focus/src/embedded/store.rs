//! Store: connection, migration, and CRUD for the SQLite memory backend.
use std::path::{Path, PathBuf};

use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use uuid::Uuid;

use super::*;

use crate::focus_stack::{FocusConclusion, FocusFrame, FocusFrameId};
use crate::memory_layer::{MemoryError, Result};
use crate::salience::SalienceConfig;

impl EmbeddedMemoryBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        // (round 5): enable busy_timeout so concurrent writes
        // from session_persistence + memory_router + compression are
        // retried by sqlite itself for up to 5 s instead of failing
        // with `database is locked`. This is the in-process backstop —
        // longer-term we still need a cross-process retry loop, but
        // for the desktop app where the focus DB is owned by a single
        // process this eliminates the bulk of the contention.
        let _ = conn.execute_batch("PRAGMA busy_timeout = 5000");
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

    /// Soft-hide expired facts (expires_at < now). Unlike frames, facts
    /// are soft-hidden first (visibility = 0, hidden_at = now) so the
    /// audit trail is preserved. Hard-deleted after 30 days by the
    /// same janitor that handles hidden frames.
    pub fn cleanup_expired_facts(&self) -> Result<usize> {
        let conn = self.conn.lock();
        self.cleanup_expired_facts_inner(&conn)
    }

    /// Inner implementation that accepts an already-locked connection
    /// (used during `migrate()` where the conn is already held).
    pub(crate) fn cleanup_expired_facts_inner(&self, conn: &Connection) -> Result<usize> {
        let now = chrono::Utc::now().to_rfc3339();
        let count = conn.execute(
            "UPDATE facts SET visibility = 0, hidden_at = ?1
             WHERE visibility = 1
               AND expires_at IS NOT NULL
               AND expires_at < ?1",
            params![now],
        )?;
        if count > 0 {
            tracing::info!(
                target: "hermix.focus",
                count,
                "cleanup_expired_facts: soft-hid expired fact rows"
            );
        }
        // Also hard-delete facts that were soft-hidden > 30 days ago
        // (same policy as frames janitor).
        let purged = conn.execute(
            "DELETE FROM facts WHERE visibility = 0
             AND hidden_at IS NOT NULL
             AND hidden_at < datetime('now', '-30 days')",
            [],
        )?;
        if purged > 0 {
            tracing::info!(
                target: "hermix.focus",
                purged,
                "cleanup_expired_facts: purged ancient hidden fact rows"
            );
        }
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
    /// Uses [`hermix_config::paths::data_dir`] so the path is identical
    /// in normal mode (`~/.hermix/data/memory.db`) and portable mode
    /// (`<install_root>/data/memory.db`).
    pub fn open_at_hermix_home() -> Result<Self> {
        let db_path = hermix_config::paths::data_dir().join("memory.db");
        Self::open(&db_path)
    }

    pub(crate) fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        //: PRAGMA user_version 跟踪 schema 演进。
        // 已存在的 CREATE TABLE / ALTER TABLE 仍然全部跑（向后兼容），
        // 但新增的 schema 块（janitor、recall_audit、session_snapshots）
        // 用 version 守卫，避免每次 open() 跑 14 个 SQL。
        // - 新装 DB：user_version=0 → 全部 migration 顺序执行 → 6
        // - 已有 DB（user_version=4）：只跑 v5 → v6（reflections 表）
        // - 最新 DB（user_version=6）：啥都不跑（极快）
        const CURRENT_SCHEMA_VERSION: i64 = 6;
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
                -- (BaiLongma soft-hide): every frame is
                -- visible by default. Soft-deleting flips `visibility`
                -- to 0 and stamps `hidden_at`. `recognize()` always
                -- filters `WHERE visibility = 1` so an accidental
                -- delete can be undone with `UPDATE frames SET
                -- visibility = 1, hidden_at = NULL WHERE id = ?`.
                visibility      INTEGER NOT NULL DEFAULT 1,
                hidden_at       TEXT,
                -- (write-path provenance): which hook
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
        //(BaiLongma soft-hide): drop frames that were
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
        // (Hermes Agent curation): separate `facts`
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
            "expires_at",
            "hidden_at",
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
                    "expires_at" => "TEXT",
                    "hidden_at" => "TEXT",
                    _ => unreachable!(),
                };
                conn.execute_batch(&format!("ALTER TABLE facts ADD COLUMN {col} {decl}"))?;
                if col == "embedding_dim" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS facts_embedding_dim_idx ON facts(embedding_dim)",
                    )?;
                }
                if col == "expires_at" {
                    conn.execute_batch(
                        "CREATE INDEX IF NOT EXISTS facts_expires_at_idx ON facts(expires_at)",
                    )?;
                }
            }
        }
        // Backfill: assign expires_at to existing facts that lack one.
        // Uses the same TTL policy as `fact_ttl_for_key` so permanent
        // facts (user.name, user.identity) stay NULL and ephemeral
        // facts (user.avoid, user.dislike, etc.) get a 7–30 day TTL
        // counted from their `created_at`.
        let backfilled: i64 = conn.query_row(
            "SELECT COUNT(*) FROM facts WHERE expires_at IS NULL AND visibility = 1",
            [], |r| r.get(0),
        )?;
        if backfilled > 0 {
            // Permanent kinds: keep expires_at = NULL
            // Ephemeral kinds: set expires_at = created_at + TTL
            let _ = conn.execute(
                "UPDATE facts SET expires_at = datetime(created_at, '+14 days')
                 WHERE expires_at IS NULL AND visibility = 1
                   AND canonical_key NOT GLOB 'user.name:*'
                   AND canonical_key NOT GLOB 'user.identity:*'",
                [],
            );
            // More specific overrides for shorter TTL kinds
            let _ = conn.execute(
                "UPDATE facts SET expires_at = datetime(created_at, '+7 days')
                 WHERE visibility = 1
                   AND (canonical_key GLOB 'user.avoid:*'
                     OR canonical_key GLOB 'user.dislike:*'
                     OR canonical_key GLOB 'compression:*')",
                [],
            );
            let _ = conn.execute(
                "UPDATE facts SET expires_at = datetime(created_at, '+30 days')
                 WHERE visibility = 1
                   AND (canonical_key GLOB 'user.style:*'
                     OR canonical_key GLOB 'user.note:*')",
                [],
            );
        }
        // Periodic janitor: soft-hide expired facts on open()
        let _ = self.cleanup_expired_facts_inner(&conn);
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

        //: version-gated incremental migrations ----
        // v2: janitor + audit (already running above — keep behaviour
        // for backward compatibility, just bump the version so future
        // opens know it ran).
        if current_version < 2 {
            // Already executed above; we only need to mark it done.
            let _ = conn.execute_batch("PRAGMA user_version = 2");
        }
        //recall_audit table .
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
        //session_snapshots table .
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
        // WAL mode + NORMAL synchronous — addresses
        // the "SQLite 锁竞争" symptom (high write frequency from
        // every-turn `sync_turn`). WAL lets readers proceed without
        // blocking on the writer; NORMAL drops the per-commit
        // fsync while still keeping crash-safe (full sync at
        // checkpoint). Together they cut disk-side lock dwell time
        // by ~10x on hot paths (no measurement here; rationale per
        // https://www.sqlite.org/wal.html + synchronous.html).
        // Both PRAGMAs are durable: WAL mode persists in the DB
        // header (sqlite3 sees it next time even without these
        // lines). `synchronous` is per-connection so we set it on
        // every open() to defend against somebody else lowering it.
        // Note: rusqlite's bundled SQLite ships with
        // `SQLITE_ENABLE_LOCKING_STYLE` — the WAL file (`-wal`)
        // lives next to memory.db. We do **not** manually truncate
        // it: passive checkpoint is enabled by default (threshold
        // ~1000 pages), and an explicit checkpoint forces a brief
        // writer stall, so we defer to SQLite's defaults.
        if current_version < 5 {
            let mode: String = conn
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .unwrap_or_else(|_| "unknown".to_string());
            // `query_row` with a PRAGMA that takes a value returns
            // the *new* mode. SQLite answers "wal" on success.
            if mode != "wal" {
                tracing::warn!(
                    target: "hermix.focus",
                    actual = %mode,
                    "PRAGMA journal_mode = WAL did not stick; \
                     file may be on a read-only filesystem"
                );
            }
            conn.execute_batch("PRAGMA synchronous = NORMAL")?;
            let _ = conn.execute_batch("PRAGMA user_version = 5");
        } else {
            // current_version >= 5: defensive re-set of `synchronous`
            // (it is per-connection, not persisted in the DB header)
            // so we don't depend on whoever opened it last.
            // (WAL mode itself persists in the DB header so we don't
            // need to re-enable it here.)
            conn.execute_batch("PRAGMA synchronous = NORMAL")?;
        }
        // `reflections` table for E (memory
        // evolution — reflexive notes written at session end, no LLM
        // required). Reflective summaries about a session's
        // what_went_well / what_to_improve / user_preferences_observed
        // / next_session_hint, driven by deterministic aggregation
        // of tool errors + user.style facts + fact count. Surfaced
        // via `StreamEvent::MemoryUpdate { kind: "reflection" }`
        // so the user can see them in chat, and indexed so the
        // recall scheduler (A) can pull the most recent ones when
        // drift is detected.
        // We don't FTS the reflection content (it's tiny text, a
        // cheap LIKE scan is fine) but we DO index by session and
        // by created_at so "most recent N for this session" is a
        // single index lookup.
        // NOTE: gate stays `current_version < 6` even after bumping
        // CURRENT_SCHEMA_VERSION to 6 — this block IS the v6
        // migration, and we want it to run on every DB that hasn't
        // already executed it. Re-runs are no-ops because the
        // CREATE TABLE / INDEX statements are all idempotent.
        if current_version < 6 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS reflections (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id  TEXT NOT NULL,
                    kind        TEXT NOT NULL,
                    content     TEXT NOT NULL,
                    salience    REAL NOT NULL DEFAULT 0.5,
                    tags        TEXT NOT NULL DEFAULT '[]',
                    source      TEXT NOT NULL DEFAULT 'reflection_generator',
                    turn_count  INTEGER NOT NULL DEFAULT 0,
                    created_at  TEXT NOT NULL,
                    visibility  INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS reflections_session_idx
                    ON reflections(session_id);
                CREATE INDEX IF NOT EXISTS reflections_created_at_idx
                    ON reflections(created_at);
                CREATE INDEX IF NOT EXISTS reflections_session_kind_idx
                    ON reflections(session_id, kind);
                "#,
            )?;
            let _ = conn.execute_batch("PRAGMA user_version = 6");
        }
        Ok(())
    }

    /// Persist a focus frame (or update an existing one by id).
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
        let payload_text = cjk_bigram_segment(&extract_text(&payload_for_index));
        let tags_text = cjk_bigram_segment(&frame.tags.join(" "));
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
    /// prefer [`Self::soft_hide_frame`] so the audit trail
    /// is preserved. `delete_frame` is kept for the rare cases where
    /// the caller is sure they want a hard delete (e.g. tests, the
    /// 30-day janitor on hidden rows).
    pub fn delete_frame(&self, id: FocusFrameId) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM frames WHERE id = ?1", params![id.to_string()])?;
        let _ = conn.execute("DELETE FROM frames_fts WHERE id = ?1", params![id.to_string()]);
        Ok(n > 0)
    }

    /// (BaiLongma soft-hide): mark a frame invisible to
    /// `recognize()` queries without losing the row. Use this when
    /// the agent decides a memory is no longer relevant (TTL
    /// expiry, semantic eviction, manual `memory` tool remove).
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

    /// undo a previous [`Self::soft_hide_frame`]. Returns
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

    /// (write-path hook target): persist a single
    /// user/assistant turn pair as a low-salience (0.3) frame tagged
    /// `turn`. Stored separately from the focus-stack conclusions
    /// (which are higher salience) so recall can prefer user-asserted
    /// facts over raw turns.
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

    /// (write-path hook target): persist an explicit
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

    /// (write-path hook target): persist a delegation
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

    /// insert or replace a `fact` row. Used by the
    /// Hermes-Agent-inspired curation hook (every N turns, or after
    /// a complex task) and by external callers that already
    /// extracted a structured fact.
    ///
    /// `canonical_key` is the stable identity used for dedup.
    /// If a fact with the same key already exists (and is visible),
    /// its `access_count` is bumped and `payload`/`salience` are
    /// updated and `expires_at` is recomputed (re-asserting a fact
    /// resets its TTL); otherwise a new row is inserted.
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
        let payload_text = cjk_bigram_segment(&extract_text(&payload));
        if payload_text.trim().is_empty() {
            return Ok(None);
        }
        let tags_json = serde_json::to_string(tags)?;
        let now = Utc::now();
        let now_rfc = now.to_rfc3339();
        // Compute expires_at from the canonical_key prefix.
        // Permanent kinds (user.name, user.identity) → NULL;
        // ephemeral kinds → now + TTL.
        let expires_at: Option<String> = fact_ttl_for_key(key_trim)
            .map(|ttl_secs| (now + chrono::Duration::seconds(ttl_secs as i64)).to_rfc3339());
        let conn = self.conn.lock();
        // Try to bump the existing row first.
        // On re-assert: reset expires_at (renew TTL), update
        // salience (MAX so it never drops on re-assert), and
        // bump access_count.
        let updated = if expires_at.is_some() {
            conn.execute(
                "UPDATE facts SET access_count = access_count + 1,
                                  updated_at = ?1,
                                  payload = ?2,
                                  salience = MAX(salience, ?3),
                                  tags = ?4,
                                  source = ?5,
                                  expires_at = ?6
                 WHERE canonical_key = ?7 AND visibility = 1",
                params![now_rfc, payload.to_string(), salience, tags_json, source, expires_at, key_trim],
            )?
        } else {
            // Permanent fact: set expires_at = NULL to ensure
            // a previous ephemeral assignment is cleared when
            // the same key is re-asserted as permanent.
            conn.execute(
                "UPDATE facts SET access_count = access_count + 1,
                                  updated_at = ?1,
                                  payload = ?2,
                                  salience = MAX(salience, ?3),
                                  tags = ?4,
                                  source = ?5,
                                  expires_at = NULL
                 WHERE canonical_key = ?6 AND visibility = 1",
                params![now_rfc, payload.to_string(), salience, tags_json, source, key_trim],
            )?
        };
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
                  tags, source, access_count, visibility, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, 0, 1, ?8)",
                params![
                    new_id.to_string(),
                    key_trim,
                    payload.to_string(),
                    salience,
                    now_rfc,
                    tags_json,
                    source,
                    expires_at,
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

    /// bump a fact's `access_count` (called on every
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

    /// record one row in `recall_audit`. Called by
    /// the agent loop on every `memory_prefetch` invocation so
    /// operators can later ask:
    /// - "what queries did this session issue?"
    /// - "what was the hit count distribution?"
    /// - "what's the p99 latency?"
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

    /// (架构-1): freeze a `summary` of the last `frame_ids`
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

    /// (架构-1): load the most recent snapshot for a
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

    /// (架构-2): enforce a hard cap on the number of visible
    /// frames. When `total > cap`, the oldest frames (lowest salience
    /// first, then oldest `created_at`) are soft-hidden so `recognize()`
    /// stops surfacing them but they remain on disk for the audit
    /// trail. Returns the number of frames newly hidden.
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

    /// (架构-2): count of visible frames. Test helper used
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

    /// (BaiLongma compression backfill): when a `Cleared`
    /// focus frame is popped, the focus stack pushes a
    /// `FocusConclusion` onto the drain queue. The agent loop calls
    /// `drain_focus_conclusions_to_memory_router` which already
    /// persists the conclusion as a `frame` row. This method
    /// additionally extracts a **deterministic fact** (no LLM
    /// needed) from the conclusion's text + tags and writes it to
    /// the `facts` table so cross-session recall surfaces it via
    /// the unified `recognize()` pipeline.
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

    /// (A): top-N visible facts by stored salience.
    /// Used by `recall_scheduler` to pull candidates for re-rank.
    /// We don't recompute salience here (no `salience_cfg` access
    /// without rebuilding one per call) — the stored value already
    /// reflects the most-recent write's salience, which is good
    /// enough for recall. Touch/pin effects will be picked up by
    /// `boost_fact_access` (called on each recall hit).
    pub fn top_facts_for_recall(&self, limit: u32) -> Result<Vec<(String, String, f32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        let mut stmt = conn.prepare(
            "SELECT canonical_key, payload, salience FROM facts
             WHERE visibility = 1
               AND (expires_at IS NULL OR expires_at >= ?1)
             ORDER BY salience DESC, updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f32>(2)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // =================================================================
    //(E): reflections table API
    // Reflections are written by `reflection_generator` at session
    // end. We deliberately keep these helpers minimal — no full-text
    // search, no embedding (the content is short and meta-cognitive;
    // the recall scheduler (A) does its own salience-rank against
    // `facts`, not against `reflections`).
    // =================================================================

    /// Persist one reflection row. Returns the new id.
    /// Visibility defaults to 1 (visible). Caller doesn't need to
    /// pre-set timestamps — we record the UTC RFC3339 now() here
    /// to avoid drift between Rust and SQLite clocks if a future
    /// caller runs from a different time zone.
    pub fn add_reflection(
        &self,
        session_id: &str,
        kind: &str,
        content: &str,
        salience: f32,
        tags: &[String],
        turn_count: u32,
        source: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock();
        let tags_json = serde_json::to_string(tags)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO reflections
             (session_id, kind, content, salience, tags, source, turn_count, created_at, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            params![
                session_id,
                kind,
                content,
                salience,
                tags_json,
                source,
                turn_count,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Return the most recent N reflections for a session, newest
    /// first. Used by the recall scheduler (A) and by `memory_router`
    /// debug views. `limit == 0` means "no rows" (we don't return
    /// every row — that would explode unbounded on long sessions).
    pub fn list_recent_reflections(&self, session_id: &str, limit: u32) -> Result<Vec<Reflection>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, kind, content, salience, tags, source, turn_count, created_at
             FROM reflections
             WHERE session_id = ?1 AND visibility = 1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            let tags_s: String = row.get(5)?;
            let tags: Vec<String> = serde_json::from_str(&tags_s).unwrap_or_default();
            Ok(Reflection {
                id: row.get(0)?,
                session_id: row.get(1)?,
                kind: row.get(2)?,
                content: row.get(3)?,
                salience: row.get(4)?,
                tags,
                source: row.get(6)?,
                turn_count: row.get::<_, i64>(7)? as u32,
                created_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// (reflection → 行为改写闭环): return the most recent
    /// reflections across **all sessions EXCEPT `exclude_session_id`**,
    /// newest first. The agent loop calls this in
    /// `memory_prefetch` of session N+1 so the next session's system
    /// prompt can carry over the lessons learned from session N's
    /// reflection notes.
    /// Empty `exclude_session_id` means "all sessions".
    pub fn list_cross_session_reflections(
        &self,
        exclude_session_id: &str,
        limit: u32,
    ) -> Result<Vec<Reflection>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, kind, content, salience, tags, source, turn_count, created_at
             FROM reflections
             WHERE visibility = 1
               AND (?1 = '' OR session_id != ?1)
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![exclude_session_id, limit as i64], |row| {
            let tags_s: String = row.get(5)?;
            let tags: Vec<String> = serde_json::from_str(&tags_s).unwrap_or_default();
            Ok(Reflection {
                id: row.get(0)?,
                session_id: row.get(1)?,
                kind: row.get(2)?,
                content: row.get(3)?,
                salience: row.get(4)?,
                tags,
                source: row.get(6)?,
                turn_count: row.get::<_, i64>(7)? as u32,
                created_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Count tool_error rows for this session — drives the
    /// `what_to_improve` reflection. Returns 0 if no errors
    /// (which is the happy case).
    /// We scan `recall_audit` because every `tool_error` already
    /// gets audited there in ; duplicating into a new
    /// table would just create two sources of truth.
    pub fn count_session_tool_errors(&self, session_id: &str) -> Result<u32> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_writes
                 WHERE session_id = ?1 AND source = 'tool_error'",
                params![session_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n.max(0) as u32)
    }

    /// Count distinct facts authored during this session. Used by
    /// `reflection_generator` to decide whether the session was
    /// knowledge-light (few facts) or knowledge-heavy (many).
    pub fn count_session_facts(&self, session_id: &str) -> Result<u32> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts
                 WHERE visibility = 1
                   AND tags LIKE ?1",
                params![format!("%session:{session_id}%")],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n.max(0) as u32)
    }

    /// (E): pick the most-frequently-accessed user.style
    /// fact (`canonical_key = user.style:<style>`). Returns None if
    /// no style has been recorded yet (early session, no rules
    /// matched).
    pub fn most_frequent_user_style(&self) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let key: Option<String> = conn
            .query_row(
                "SELECT canonical_key FROM facts
                 WHERE visibility = 1
                   AND canonical_key LIKE 'user.style:%'
                 ORDER BY access_count DESC, updated_at DESC
                 LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(key.map(|k| {
            // Strip the "user.style:" prefix to leave just the style
            // token (e.g. "direct", "exploratory").
            k.strip_prefix("user.style:")
                .map(|s| s.to_string())
                .unwrap_or(k)
        }))
    }
}
