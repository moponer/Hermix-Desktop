//! DB connection, WAL setup, migrations, and version stamping.
use std::path::Path;

use hermix_core::AgentError;
use rusqlite::OptionalExtension;

use super::*;

impl SessionPersistence {
    /// Create a new persistence manager rooted at the given hermes home directory.
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
        // The CREATE TABLE / CREATE INDEX block below references the
        // `active` and `extra` columns. On a pre-migration database those
        // columns don't exist yet, and `CREATE INDEX ... ON
        // messages(session_id, active)` will fail with
        // `no such column: active` — the entire `execute_batch` then
        // bails and the ALTER TABLE migrations that would have added the
        // column never run. So: do the migrations first, then the
        // CREATE statements that depend on the migrated schema.
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

        //Migration : add `name` column for tool messages.
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

        //Migration : add `workspace` and `expert_id` columns
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

        //Migration : add `parent_session_id` column for
        // session lineage tracking. Allows the frontend to display a
        // tree of parent/child sessions (e.g. root session → sub-agent
        // sessions). `None` for top-level sessions.
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.parent_session_id: {e}"
                )));
            }
        }

        //Migration : add `last_context_usage` column to
        // persist the most recent context-usage snapshot so a restored
        // session shows the context-usage bar without a fresh stream.
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN last_context_usage TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.last_context_usage: {e}"
                )));
            }
        }

        // Migration: add `workbench_id` so a session knows which workbench
        // (if any) it belongs to. NULL is an ordinary chat; non-NULL marks a
        // workbench-bound session, letting the main sidebar exclude it and
        // the workbench sidebar scope to its own id. Without this both
        // conversation spaces draw from one flat pool and leak into each
        // other's lists.
        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN workbench_id TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.workbench_id: {e}"
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
        // Migration: unified_steps (frontend-built chronological
        // reasoning/tools/text timeline). Without it, a session reload
        // can't restore the inline tool interleaving the user saw
        // mid-stream and falls back to the fixed reasoning→tools→text
        // order (tools grouped at the end). Serialized as a JSON TEXT
        // blob (array of {kind,...} step objects) — the server stores
        // it verbatim and the client feeds it straight into
        // `resolveSteps`. Idempotent via "duplicate column" detection.
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN unified_steps TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("no such table") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.unified_steps: {e}"
                )));
            }
        }

        // -----------------------------------------------------------------
        // schema-version migration bookkeeping. This table is
        // the single source of truth for "which schema migrations has
        // this database already applied?". Pre-existing DBs are stamped
        // with the migrations they're known to have (the ALTER TABLE
        // columns above and the trigram FTS5 virtual table + triggers
        // below). New DBs start at the current version.
        // Why not just rely on `CREATE TABLE / TRIGGER IF NOT EXISTS`?
        // Because `IF NOT EXISTS` is binary — either the object exists
        // (skip) or it doesn't (create). It can't express "this
        // migration needs to apply only if it hasn't been applied to
        // *this specific DB*". A user who upgraded past a buggy
        // intermediate state (e.g. trigram existed in the partial
        // migration but the triggers didn't) would silently keep a
        // broken search index until they wiped the DB.
        // The tracking table makes every migration idempotent at the
        // application layer rather than the SQL layer.
        // -----------------------------------------------------------------
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            );"#,
        )
        .map_err(|e| AgentError::Io(format!("schema_migrations create: {e}")))?;

        // Stamp legacy DBs with the migrations they're known to have
        // (added before the tracking table existed). The list mirrors
        // every ALTER TABLE / CREATE INDEX that ran in this function
        //before ; adding a new migration requires
        // adding a new entry here AND writing the corresponding
        // migration code above.
        stamp_migration(&conn, 1, "initial_schema_sessions_messages_fts", None)?;
        stamp_migration(&conn, 2, "messages_active_extra_name", None)?;
        stamp_migration(&conn, 3, "sessions_workspace_expert_parent", None)?;
        stamp_migration(&conn, 4, "messages_execution_timeline_reasoning", None)?;
        stamp_migration(
            &conn,
            5,
            "messages_fts_trigram",
            Some("CREATE_VIRTUAL_TABLE messages_fts_trigram"),
        )?;
        //Orchestration Engine tables (tasks + automation_*)
        stamp_migration(&conn, 6, "orchestration_engine_tables", None)?;
        // token_usage cache + reasoning columns.
        // The actual ALTER TABLE lives below the CREATE TABLE block
        // because the table must exist before we can ADD COLUMN. The
        // stamp here records the migration as applied so the ALTER
        // doesn't fire on subsequent opens (the ALTER itself is also
        // idempotent via "duplicate column name" detection — defense
        // in depth).
        stamp_migration(&conn, 7, "token_usage_cache_reasoning_columns", None)?;
        // unified_steps (frontend-built reasoning/tools/text timeline).
        // The ALTER TABLE lives just above the schema-version bookkeeping
        // block; this stamp records the migration as applied so a
        // re-open doesn't re-fire it (the ALTER is also idempotent via
        // "duplicate column" detection — defense in depth).
        stamp_migration(&conn, 8, "messages_unified_steps", None)?;

        // P5 L3 seq 域（设计 §3.5）：每 session 独立持久 seq 计数器。
        // 重连订阅端点（`GET /v1/sessions/{id}/subscribe?since_seq=N`）
        // 的补发增量以此为权威源——进程级 AtomicU64（重启归零）只能做
        // 实时广播锚，跨重启对账必须落到这里。`seq` 单调递增、允许
        // 间隙（客户端只做大小比较）。首条 seq = 1（SQLite 无 0 号序）。
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS session_seq (
                session_id TEXT PRIMARY KEY,
                last_seq   INTEGER NOT NULL DEFAULT 0
            );"#,
        )
        .map_err(|e| AgentError::Io(format!("session_seq create: {e}")))?;
        stamp_migration(&conn, 9, "session_seq_table", None)?;

    /// Record a migration as applied (idempotent). When `marker_sql` is
    /// `Some`, the marker SQL is also executed so future upgrades can
    /// replay the side-effect even if a previous version wrote the
    /// tracking row but skipped the DDL (e.g. partial migration
    /// recovery).
    pub(crate) fn stamp_migration(
        conn: &rusqlite::Connection,
        version: i64,
        name: &str,
        marker_sql: Option<&str>,
    ) -> Result<(), AgentError> {
        // Detect existing row by version (PK, cheap).
        let already: bool = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [version],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| AgentError::Io(format!("schema_migrations lookup: {e}")))?
            .unwrap_or(false);

        if already {
            return Ok(());
        }

        // Replay the marker SQL if the caller provided one and the
        // `IF NOT EXISTS` style statement hasn't already created it.
        if let Some(sql) = marker_sql {
            let _ = conn.execute_batch(sql);
        }

        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, datetime('now'))",
            rusqlite::params![version, name],
        )
        .map_err(|e| AgentError::Io(format!("schema_migrations insert v{version}: {e}")))?;
        Ok(())
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
                compressed_summary TEXT,
                parent_session_id TEXT,
                workspace TEXT,
                expert_id TEXT,
                last_context_usage TEXT
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
                unified_steps TEXT,
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

            CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, session_id, role)
                VALUES ('delete', old.id, old.content, old.session_id, old.role);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, session_id, role)
                VALUES ('delete', old.id, old.content, old.session_id, old.role);
                INSERT INTO messages_fts(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;

            -- Trigram tokenizer for CJK / substring search (Hermes Agent parity).
            -- The default FTS5 tokenizer cannot match Chinese/Japanese/Korean
            -- characters or arbitrary substrings. The trigram tokenizer indexes
            -- every 3-character contiguous run, enabling LIKE-style searches
            -- over CJK content without external extensions.
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts_trigram USING fts5(
                content,
                session_id UNINDEXED,
                role UNINDEXED,
                content='messages',
                content_rowid='id',
                tokenize='trigram'
            );

            CREATE TRIGGER IF NOT EXISTS messages_trigram_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts_trigram(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_trigram_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts_trigram(messages_fts_trigram, rowid, content, session_id, role)
                VALUES ('delete', old.id, old.content, old.session_id, old.role);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_trigram_au AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts_trigram(messages_fts_trigram, rowid, content, session_id, role)
                VALUES ('delete', old.id, old.content, old.session_id, old.role);
                INSERT INTO messages_fts_trigram(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;

            -- (A): append-only archive of the *raw* turns that
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

            -- (B): append-only summary history. The
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

            -- append-only LLM token usage log.
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
                -- provider cache token breakdown so /insights
                -- can surface real cache hit rates instead of zeros.
                -- All three default to 0 so providers that don't report
                -- cache stats (e.g. MiniMax-M3 today) don't break the
                -- INSERT path.
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost REAL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_token_usage_created_at
                ON token_usage(created_at);
            CREATE INDEX IF NOT EXISTS idx_token_usage_model
                ON token_usage(model);
            CREATE INDEX IF NOT EXISTS idx_token_usage_session
                ON token_usage(session_id, created_at);

            -- ── Orchestration Engine tables ──────
            -- All three tables live in sessions.db to share the same
            -- WAL + busy_timeout (铁律 D/W). No cross-file journal
            -- contention because there is only one file.

            -- Task table: top-level work unit carrying workspace,
            -- conversation context, mode (Craft/Plan/Ask), and
            -- deliverable result references.
            CREATE TABLE IF NOT EXISTS tasks (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                workspace_path  TEXT NOT NULL,
                mode            TEXT NOT NULL DEFAULT 'craft',
                status          TEXT NOT NULL DEFAULT 'pending',
                parent_task_id  TEXT,
                context_json    TEXT NOT NULL DEFAULT '{}',
                result_refs_json TEXT NOT NULL DEFAULT '[]',
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                -- Owning session (conversation). Kept here as well as in
                -- `hermix_task::TaskRepository::ensure_schema` because
                -- whichever creator runs first decides the shape of the
                -- table: this one runs at startup, before the task
                -- repository, so leaving the column out made every fresh
                -- install look like a legacy install to the migration.
                session_id      TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);

            -- Automation definition table: user-defined scheduled tasks
            -- (recurring via RRULE or one-shot). Decoupled from the
            -- execution engine by the runtime_state + runs tables.
            CREATE TABLE IF NOT EXISTS automations (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                prompt        TEXT NOT NULL,
                schedule_type TEXT NOT NULL,
                rrule         TEXT,
                scheduled_at  TEXT,
                status        TEXT NOT NULL DEFAULT 'ACTIVE',
                valid_from    TEXT,
                valid_until   TEXT,
                cwds          TEXT,
                connector_ids_json TEXT,
                expert_id     TEXT,
                model_id      TEXT,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL
            );

            -- Automation runtime state: last/next run timestamps and
            -- run count. One row per automation (PK = automation_id).
            CREATE TABLE IF NOT EXISTS automation_runtime_state (
                automation_id TEXT PRIMARY KEY,
                last_run_at   INTEGER,
                next_run_at   INTEGER,
                run_count     INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(automation_id) REFERENCES automations(id) ON DELETE CASCADE
            );

            -- Automation run history: one row per execution of an
            -- automation. Links to the generated Task via task_id.
            CREATE TABLE IF NOT EXISTS automation_runs (
                id            TEXT PRIMARY KEY,
                automation_id TEXT NOT NULL,
                started_at    INTEGER NOT NULL,
                finished_at   INTEGER,
                status        TEXT NOT NULL,
                task_id       TEXT,
                error_text    TEXT,
                FOREIGN KEY(automation_id) REFERENCES automations(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_automation_runs_automation
                ON automation_runs(automation_id, started_at);"#,
        )
        .map_err(|e| AgentError::Io(format!("Failed to create tables: {e}")))?;

        // Migration: add compressed_summary column if missing (existing databases).
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN compressed_summary TEXT",
            [],
        );

        // token_usage cache + reasoning columns. Fresh DBs
        // got them from the CREATE TABLE above. Pre-existing DBs that
        // predate v7 need an idempotent ALTER per column; "duplicate
        // column name" is the success signal for "already migrated".
        // The stamp_migration(v7) earlier in this function prevents
        // this loop from running again on subsequent opens, but the
        // duplicate-name guard is the real safety net.
        for (col, decl) in [
            ("cache_read_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("cache_creation_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("reasoning_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let sql = format!("ALTER TABLE token_usage ADD COLUMN {col} {decl}");
            if let Err(e) = conn.execute(&sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(AgentError::Io(format!(
                        "token_usage {col} migration: {e}"
                    )));
                }
            }
        }

        Ok(())
    }
}
