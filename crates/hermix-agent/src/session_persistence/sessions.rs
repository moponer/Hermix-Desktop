//! Session persistence: token usage, session save, atomic writes.

use chrono::Utc;
use hermix_core::{AgentError, Message};

use super::*;

impl SessionPersistence {
    /// Persist ONE LLM API call's token usage to the append-only
    /// `token_usage` table. Called from `process_post_response` after
    /// every successful API response — one row per call, not per turn —
    /// so each row's numbers are the provider's verbatim report for that
    /// single request (with garbage reports already replaced by the
    /// local anchor upstream in `process_post_response`). `estimated_cost`
    /// is the per-call cost stamped by the caller when pricing is known;
    /// `untrusted` marks rows whose reported counts failed the local
    /// sanity check.
    pub fn persist_token_usage(
        &self,
        session_id: &str,
        model: &str,
        usage: &hermix_core::UsageStats,
        untrusted: bool,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;
        let now = Utc::now().to_rfc3339();
        // `token_usage.session_id` 有 FOREIGN KEY → `sessions(id)`，而子代理
        // 会话（`parent::subagent-<uuid>`）从不走 `persist_session`，直接 INSERT
        // 必然违反外键 → 整段用量静默丢失、/insights 缺一块（实测 2026-10-27）。
        //
        // 先补一行最小占位（已存在则 IGNORE 不动），保证用量落盘。只填 NOT NULL
        // 的两个时间戳：这条记录的用途是承载用量，不是出现在会话列表里，
        // 标题/模型留空比伪造一个更像占位。
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
            rusqlite::params![session_id, now],
        )
        .map_err(|e| AgentError::Io(format!("Failed to ensure session row for usage: {e}")))?;
        conn.execute(
            "INSERT INTO token_usage
                (session_id, model, prompt_tokens, completion_tokens,
                 total_tokens, cache_read_tokens, cache_creation_tokens,
                 reasoning_tokens, untrusted, estimated_cost, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                session_id,
                model,
                usage.prompt_tokens as i64,
                usage.completion_tokens as i64,
                usage.total_tokens as i64,
                usage.cache_read_tokens as i64,
                usage.cache_creation_tokens as i64,
                usage.reasoning_tokens as i64,
                untrusted as i64,
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
    /// `workspace` / `expert_id` are persisted to the `sessions` row
    /// in the same transaction so a session reload (e.g. after the
    /// desktop app restarts) restores the same workspace category
    /// the user picked in the welcome UI. Without this, the
    /// sidebar bucketed every restarted session under "未分组".
    /// Pass `None` to leave the column unchanged (the `COALESCE`
    /// inside `save_session_atomic_inner` makes the no-op explicit).
    pub fn persist_session(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        parent_session_id: Option<&str>,
        workspace: Option<&str>,
        expert_id: Option<&str>,
    ) -> Result<(), AgentError> {
        self.save_session_atomic(session_id, messages, model, platform, title, system_prompt, parent_session_id, workspace, expert_id, None, None, false, true)
    }

    /// Persist a session's messages to SQLite, optionally including a
    /// compressed context summary for cross-session continuity.
    /// Delegates to the unified [`save_session_atomic`] which wraps
    /// DB + markdown + trajectory writes in a single transaction.
    /// prefer [`persist_session_with_compression`] when
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
        parent_session_id: Option<&str>,
        compressed_summary: Option<&str>,
        workspace: Option<&str>,
        expert_id: Option<&str>,
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
            parent_session_id,
            workspace,
            expert_id,
            snapshot_ref,
            None,
            false,
            true,
        )
    }

    /// (A + B): persist a session with a full
    /// compression snapshot. The snapshot carries (a) the handoff
    /// summary, (b) the raw archived turns, and (c) the compression
    /// metrics. All three are written to their respective tables in
    /// the same transaction as the `messages` DELETE+INSERT, so the
    /// snapshot can never go out of sync with the latest summary
    /// even under a mid-write crash.
    /// When the snapshot is empty (no compression this turn), this
    /// is functionally identical to the legacy
    /// `persist_session` call.
    /// `workspace` / `expert_id` : see [`persist_session`].
    pub fn persist_session_with_compression(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        parent_session_id: Option<&str>,
        workspace: Option<&str>,
        expert_id: Option<&str>,
        compression: Option<&CompressionSnapshot>,
    ) -> Result<(), AgentError> {
        self.save_session_atomic(
            session_id,
            messages,
            model,
            platform,
            title,
            system_prompt,
            parent_session_id,
            workspace,
            expert_id,
            compression,
            None,
            false,
            true,
        )
    }

    /// Persist session + messages + markdown + trajectory in a single
    /// atomic operation. Uses SQLite `BEGIN IMMEDIATE` so a crash
    /// between the three writes leaves the database in a consistent
    /// state (no partial DELETE). File writes (md / json) are attempted
    /// after the DB COMMIT — if they fail we log a warning but do not
    /// roll back, because the SQLite DB is the single source of truth.
    /// `compression` replaces the older
    /// `compressed_summary: Option<&str>` parameter: when non-`None`,
    /// the snapshot's summary also goes to `sessions.compressed_summary`
    /// (latest-only fast-load index), the raw archived turns go to
    /// `compression_archives` (A — durable raw), and the summary +
    /// metrics go to `compression_summary_history` (B — append-only
    /// history).
    /// `workspace` / `expert_id` are written to the `sessions` row in
    /// the same transaction (COALESCE — pass `None` to leave the
    /// column unchanged). This is what makes session reloads restore
    /// the right workspace category in the desktop sidebar; without
    /// it the SQL `INSERT ... ON CONFLICT DO UPDATE SET` skipped
    /// these columns entirely and only the legacy `PATCH /v1/sessions`
    /// route could write them.
    /// `user_query` and `completed` are only needed for the trajectory
    /// file; pass `(None, false)` to skip trajectory.
    ///
    /// `write_session_log` gates the human-readable `<id>.md` export. Callers
    /// that persist twice in one turn (pre-save then post-run) pass `false`
    /// for the first one: the file is re-rendered in full from the same
    /// timeline a moment later, so writing it twice only doubles the remaining
    /// per-turn full-file write.
    #[allow(clippy::too_many_arguments)]
    pub fn save_session_atomic(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        parent_session_id: Option<&str>,
        workspace: Option<&str>,
        expert_id: Option<&str>,
        compression: Option<&CompressionSnapshot>,
        user_query: Option<&str>,
        completed: bool,
        write_session_log: bool,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;

        // Application-level retry with jitter for write contention (Hermes Agent parity).
        let mut last_err = None;
        for attempt in 0..=WRITE_MAX_RETRIES {
            match self.save_session_atomic_inner(
                session_id, messages, model, platform, title,
                system_prompt, parent_session_id, workspace, expert_id,
                compression,
                user_query, completed,
                write_session_log,
            ) {
                Ok(()) => return Ok(()),
                Err(e) if is_busy_error(&e) && attempt < WRITE_MAX_RETRIES => {
                    let jitter_ms = WRITE_RETRY_MIN_MS
                        + (rand_jitter_u64() % (WRITE_RETRY_MAX_MS - WRITE_RETRY_MIN_MS + 1));
                    tracing::warn!(
                        "session_persistence: SQLITE_BUSY on attempt {}/{}, retrying in {}ms",
                        attempt + 1, WRITE_MAX_RETRIES, jitter_ms,
                    );
                    std::thread::sleep(std::time::Duration::from_millis(jitter_ms));
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| AgentError::Io("save_session_atomic: exhausted retries".into())))
    }

    /// Persist the user's message *before* the agent runs, without the `.md`
    /// export.
    ///
    /// The pre-save exists so that a failed run (provider error / network
    /// timeout / `?` short-circuit) cannot lose what the user typed: the DB
    /// row is the part that must survive. The markdown log is re-rendered in
    /// full from the same timeline by the post-run persist, so producing it
    /// here writes the whole conversation to disk twice per turn for a file
    /// that is superseded within seconds.
    pub fn persist_pre_turn(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
    ) -> Result<(), AgentError> {
        self.save_session_atomic(
            session_id, messages, model, platform, None, None, None, None, None, None, None, false,
            // No `.md` export — see the doc comment.
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    /// persist the latest context-usage snapshot for a session.
    /// Called every agent-loop turn when a `context_usage` event is emitted,
    /// so a restored session can show the context-usage bar without waiting
    /// for a fresh live stream.
    pub fn update_context_usage(
        &self,
        session_id: &str,
        payload: &hermix_core::types::ContextUsagePayload,
    ) -> Result<(), AgentError> {
        let json = serde_json::to_string(payload)
            .map_err(|e| AgentError::Io(format!("Failed to serialize context_usage: {e}")))?;
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "UPDATE sessions SET last_context_usage = ?1 WHERE id = ?2",
            rusqlite::params![json, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to persist context_usage: {e}")))?;
        Ok(())
    }

    /// read the last persisted context-usage snapshot for a
    /// session, if any. Returns `None` when the column is missing (old DB)
    /// or empty.
    pub fn get_context_usage(&self, session_id: &str) -> Option<hermix_core::types::ContextUsagePayload> {
        let conn = match open_with_wal(&self.db_path) {
            Ok(c) => c,
            Err(_) => return None,
        };
        let row: Option<String> = conn
            .query_row(
                "SELECT last_context_usage FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        row.and_then(|s| serde_json::from_str::<hermix_core::types::ContextUsagePayload>(&s).ok())
    }

    pub(crate) fn save_session_atomic_inner(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
        parent_session_id: Option<&str>,
        workspace: Option<&str>,
        expert_id: Option<&str>,
        compression: Option<&CompressionSnapshot>,
        user_query: Option<&str>,
        completed: bool,
        write_session_log: bool,
    ) -> Result<(), AgentError> {
        let conn = open_with_wal(&self.db_path)?;

        // BEGIN IMMEDIATE — prevents other readers from seeing partial data
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| AgentError::Io(format!("Failed to begin transaction: {e}")))?;

        let summary_str = compression.map(|s| s.summary.as_str());
        let db_result = (|| -> Result<(), AgentError> {
            let now = Utc::now().to_rfc3339();

            // 1. UPSERT the session row FIRST so the messages.session_id →
            // sessions.id FOREIGN KEY holds for the INSERT in step 2.
            // We can't stamp the real `message_count` here — it depends on
            // how many timeline rows survive the flush. We use a 0 placeholder
            // for a brand-new row and patch it in step 3 with the real count
            // returned by `flush_messages`.
            //
            // `message_count` is deliberately NOT in the ON CONFLICT SET
            // clause: on an existing session the previous turn's value must
            // survive until step 3, because `flush_messages` reads it as the
            // incremental-flush watermark ("the stored timeline had N rows").
            // Adding it here would reset the watermark to 0 and silently
            // demote every turn to a full history scan.
            conn.execute(
                "INSERT INTO sessions (id, model, platform, created_at, updated_at, title, message_count, system_prompt, compressed_summary, parent_session_id, workspace, expert_id, last_context_usage)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                    model = COALESCE(?2, sessions.model),
                    updated_at = ?4,
                    title = COALESCE(?5, sessions.title),
                    system_prompt = COALESCE(?6, sessions.system_prompt),
                    compressed_summary = COALESCE(?7, sessions.compressed_summary),
                    parent_session_id = COALESCE(?8, sessions.parent_session_id),
                    workspace = COALESCE(?9, sessions.workspace),
                    expert_id = COALESCE(?10, sessions.expert_id),
                    last_context_usage = COALESCE(?11, sessions.last_context_usage)",
                rusqlite::params![
                    session_id,
                    model,
                    platform.unwrap_or("cli"),
                    now,
                    title,
                    system_prompt,
                    summary_str,
                    parent_session_id,
                    workspace,
                    expert_id,
                    None::<String>,   // last_context_usage: managed by update_context_usage()
                ],
            )
            .map_err(|e| AgentError::Io(format!("Failed to upsert session: {e}")))?;

            // 2. Merge the timeline into storage incrementally (unchanged
            // prefix kept, only the changed tail rewritten — see
            // `flush_messages`). Returns the timeline length, not the number
            // of rows this call inserted.
            let timeline_len = self.flush_messages(&conn, session_id, messages, &now)?;

            // 3. Stamp the timeline length onto `sessions.message_count`.
            // It doubles as the incremental flush's watermark: the next turn
            // reads it back to decide whether the stored rows are still a
            // verbatim prefix (see `flush_messages`' fast path). Writing a
            // partial count here would silently disable that fast path, so the
            // value must be the whole timeline.
            conn.execute(
                "UPDATE sessions SET message_count = ?1 WHERE id = ?2",
                rusqlite::params![timeline_len as i64, session_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to patch session message_count: {e}")))?;

            // 4. (A + B): compression-side writes inside the same
            // transaction. If any of these fails, the whole
            // transaction rolls back and `messages` + `sessions`
            // stay where they were — so the snapshot can never go
            // out of sync with the latest summary.
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

            // P5 L3 seq 域（设计 §3.5）：为本回合已提交的条目块分配
            // 持久 seq 号。必须在事务内（与 flush_messages 同一原子
            // 单元）——seq 是"已提交条目块"的对账锚，若落在事务外，
            // commit 失败会留下幽灵 seq（客户端收到 seq=N 的条目但
            // 其内容未持久化，重连补发时 N 号空洞）。每次成功 flush
            // 分配一个 seq（回合级，非消息级）。`completed_keys` 的
            // 旧"已完成任务短路"语义在 P5 全量批次下线，本批保留
            // 双写以维持现行行为。
            let _seq = conn
                .query_row(
                    r#"
                    INSERT INTO session_seq (session_id, last_seq)
                    VALUES (?1, 1)
                    ON CONFLICT(session_id) DO UPDATE SET last_seq = last_seq + 1
                    RETURNING last_seq
                    "#,
                    rusqlite::params![session_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| AgentError::Io(format!(
                    "Failed to allocate session seq: {e}"
                )))? as u64;

            Ok(())
        })();

        match db_result {
            Ok(()) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| AgentError::Io(format!("Failed to commit transaction: {e}")))?;

                // 3. File writes (best-effort after DB COMMIT)
                if write_session_log {
                    if let Err(e) = self.save_session_log(session_id, messages, model) {
                        tracing::warn!("Failed to save session log (non-fatal): {e}");
                    }
                }
                // JSONL 镜像（Codex rollout 双轨 analog）：DB 损坏时可按行
                // 重建时间线。增量 append，成本 O(delta)。
                if let Err(e) = self.save_message_mirror(session_id, messages) {
                    tracing::warn!("Failed to append JSONL mirror (non-fatal): {e}");
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
}
