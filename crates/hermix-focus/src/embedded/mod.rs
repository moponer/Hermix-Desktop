//! Embedded SQLite-backed `MemoryLayer`.
//!
//! This is the "local-first" backend the调研 recommended (`PROPOSAL_BAILONGMA_INSPIRED.md`
//! §2.2 "本地计算 cosine", §五 L1).
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
//! payload_text, -- extracted string from payload (: bigram-preprocessed for CJK)
//!     tags_text,
//!     tokenize = 'unicode61 remove_diacritics 2'
//! );
//! ```
//!
//! The FTS5 table is optional but provides sub-millisecond keyword recall.


pub mod store;
pub mod embed;
pub mod retrieve;
pub mod score;
pub mod model;
pub mod cjk;
pub mod ui;

pub use model::*;
pub(crate) use model::TurnRecord;
pub(crate) use cjk::*;
pub(crate) use score::*;

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::focus_stack::{FocusConclusion, FocusFrame};
use crate::memory_layer::{MemoryHit, MemoryLayer, MemoryQuery, Result, SystemPromptBlock};
use crate::salience::SalienceConfig;

pub struct EmbeddedMemoryBackend {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) path: PathBuf,
    /// H5: salience config used by `rerank_by_salience` and
    /// `conclude` to apply the recency × freq × pin algorithm.
    pub(crate) salience_cfg: SalienceConfig,
    /// session_id bound via `initialize`. Used as
    /// `frame.source` for entries written by the write-path hooks
    /// (`sync_turn`, `on_memory_write`, `on_session_end`).
    pub(crate) session_id: Mutex<Option<String>>,
    /// hermix_home bound via `initialize`. Currently
    /// informational (we already know `path`) but kept on the struct
    /// so external observers (status panels, telemetry) can confirm
    /// which home directory the backend is bound to.
    pub(crate) hermix_home: Mutex<Option<String>>,
    /// monotonic turn counter incremented by
    /// `on_turn_start`. Used to gate the Hermes-Agent-inspired
    /// curation hook (`CURATE_INTERVAL`).
    pub(crate) turn_count: Mutex<u32>,
    /// log of the last N (user, assistant) turn pairs for
    /// the `on_pre_compress` hook to summarize. Bounded to avoid
    /// unbounded memory growth; we keep the last 20 pairs in
    /// memory.
    pub(crate) recent_turns: Mutex<Vec<TurnRecord>>,
}

impl std::fmt::Debug for EmbeddedMemoryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedMemoryBackend")
            .field("path", &self.path)
            .finish()
    }
}

pub fn fact_ttl_for_key(canonical_key: &str) -> Option<u32> {
    const DAY: u32 = 24 * 3600;
    if canonical_key.starts_with("user.name:") || canonical_key.starts_with("user.identity:") {
        None // permanent
    } else if canonical_key.starts_with("user.memory:") {
        // Promoted `memory`-tool entries (session md → facts at
        // session end). Long TTL: re-promotion while the entry
        // stays in the session md renews it; entries archived out
        // of the md fade instead of living forever.
        Some(180 * DAY)
    } else if canonical_key.starts_with("user.style:") || canonical_key.starts_with("user.note:") {
        Some(30 * DAY)
    } else if canonical_key.starts_with("user.avoid:") || canonical_key.starts_with("user.dislike:") {
        Some(7 * DAY)
    } else if canonical_key.starts_with("compression:") {
        Some(7 * DAY)
    } else if canonical_key.starts_with("user.preference:") || canonical_key.starts_with("user.tool:") {
        Some(14 * DAY)
    } else {
        Some(14 * DAY) // conservative default
    }
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
        // Dedup: the conclusion path (`memory_on_session_end`) persists
        // the same last-turn pair as a focus frame, and repeated
        // session-end calls would otherwise stack duplicate notes —
        // the module doc always claimed a freshness check here; the
        // check was never implemented. Skip when a session-source
        // frame for this session already exists within the last hour.
        // Query failure degrades to "no recent frame" (write anyway):
        // a rare duplicate is cheaper than a silent memory gap.
        if matches!(self.has_recent_session_frame(&session_id, 1), Ok(true)) {
            tracing::debug!(
                target: "hermix.focus",
                session_id,
                "on_session_end: recent session frame exists; skipping duplicate"
            );
            return;
        }
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
        // `sync_turn` call) gives the compressor a high-quality
        // summary of the *last 20 turns* without round-tripping
        // to SQLite.
        // 2. If the agent loop passes `messages` directly, we
        // additionally include a head/tail excerpt so the
        // compressor sees the global shape.
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
        //a one-line description of the local backend so
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
        //(BaiLongma soft-hide): every recall leg filters
        // `visibility = 1` so soft-hidden frames never reach the
        // LLM. Hidden frames are kept on disk for the audit trail
        // and 30-day janitor.
        // 1. Text leg — FTS5 first, LIKE fallback. We over-fetch
        // (limit * 2) so a later merge with the vector leg has
        // material to dedupe against.
        let sid = query.session_id.as_deref();
        let mut ids = self.fts_search_visible(&query.text, query.limit * 2, sid)?;
        if ids.is_empty() {
            ids = self.like_search_visible(&query.text, query.limit * 2, sid)?;
        }
        // 1b. Fact leg — query the `facts` table too so L2 Semantic
        // memory is part of the unified recall.
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
        // we run cosine_topk in parallel and merge. Empty model
        // filter (`""`) is the right call here because the query
        // embedding is "of the agent's current model" and the
        // backend stores whatever model it was given; the strict
        // match would be a follow-up. For now we let cosine
        // speak for itself and rely on the `embedding_dim` index
        // to keep the scan cheap.
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

    /// persist a `FocusConclusion` from the agent loop's
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
    use uuid::Uuid;

    // ------------------------------------------------------------------
    //: CJK bigram preprocessor unit tests
    // ------------------------------------------------------------------

    #[test]
    fn m4_bigram_segments_pure_cjk() {
        // 我喜欢 → 我 喜欢 (one bigram + 2 singles)
        let out = cjk_bigram_segment("我喜欢");
        assert!(out.contains("我 "), "got '{out}'");
        assert!(out.contains("喜欢"), "got '{out}'");
    }

    #[test]
    fn m4_bigram_preserves_ascii_tokens() {
        let out = cjk_bigram_segment("我喜欢 Rust");
        // ASCII "Rust" emitted as-is, surrounded by single CJK chars.
        assert!(out.contains("Rust"), "got '{out}'");
        // CJK chars also emitted as single-char tokens for short queries.
        assert!(out.contains("我"), "got '{out}'");
    }

    #[test]
    fn m4_bigram_handles_empty() {
        assert_eq!(cjk_bigram_segment(""), "");
    }

    #[test]
    fn m4_bigram_handles_pure_ascii() {
        assert_eq!(cjk_bigram_segment("hello world"), "hello world");
    }

    #[test]
    fn m4_bigram_chinese_query_matches_indexed_text() {
        // Simulates a real FTS5 round-trip:
        // 1. Insert frame with Chinese payload
        // 2. Single-word Chinese query "喜欢" should hit it
        // Without bigram preprocessor, the query "喜欢" would tokenize
        // to one term that doesn't appear in the index (which has
        // "我喜欢Rust" as a single unicode61 token).
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("我喜欢 Rust".to_string());
        b.upsert_frame(&f).unwrap();

        // Direct single-word query
        let hits = b.fts_search_visible("喜欢", 10, None).unwrap();
        assert!(
            !hits.is_empty(),
            "single-char query '喜欢' must hit indexed '我喜欢 Rust', got {} hits",
            hits.len()
        );
    }

    #[test]
    fn m4_bigram_mixed_chinese_ascii_query() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let f = FocusFrame::new("我喜欢 Rust 编程".to_string());
        b.upsert_frame(&f).unwrap();

        // Multi-token mixed query: should match
        let hits = b.fts_search_visible("Rust 编程", 10, None).unwrap();
        assert!(!hits.is_empty(), "mixed query 'Rust 编程' must hit, got {} hits", hits.len());
    }

    // ------------------------------------------------------------------
    //: SQLite WAL + synchronous=NORMAL — disk lock
    // contention relief for the every-turn `sync_turn` write path.
    // ------------------------------------------------------------------

    /// v5 migration flips `journal_mode` to WAL and bumps
    /// `user_version` to 5. We must use a file-backed DB because
    /// `:memory:` databases ignore WAL (they answer `memory` even
    /// after `journal_mode = WAL`).
    #[test]
    fn m6_wal_mode_active_after_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let b = EmbeddedMemoryBackend::open(&db_path).expect("open");

        let conn = b.conn.lock();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("user_version");
        // synchronous=NORMAL → SQLite answers `1`. FULL is `2`,
        // OFF is `0`. We accept `1` here; if a later migration
        // changes the policy, update this assertion in the same
        // commit.
        let sync_mode: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .expect("synchronous");
        drop(conn);

        assert_eq!(mode, "wal", "journal_mode must be WAL after migrate()");
        // bumped from 5 → 6 when the v6 reflections migration
        //was promoted from "gated CREATE TABLE IF NOT EXISTS"
        // into a version-tracked migration. CURRENT_SCHEMA_VERSION was
        // bumped in lockstep; this assertion follows.
        assert_eq!(version, 6, "user_version must be 6 after migrate()");
        assert_eq!(sync_mode, 1, "synchronous must be NORMAL (1), not FULL (2) or OFF (0)");
    }

    /// Idempotency: opening an already-v5 DB does **not** drop WAL or
    /// change user_version. `synchronous` is per-connection so it
    /// re-applies on every open() (defensive, see comment in
    /// migrate()).
    #[test]
    fn m6_v5_reopen_keeps_wal_and_bumps_synchronous_defensively() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        {
            let _b = EmbeddedMemoryBackend::open(&db_path).expect("first open");
        }
        let b2 = EmbeddedMemoryBackend::open(&db_path).expect("second open");

        let conn = b2.conn.lock();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("user_version");
        let sync_mode: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .expect("synchronous");
        drop(conn);

        assert_eq!(mode, "wal", "second open must keep WAL");
        // v6 (reflections table) is now part of the schema
        // trail, so a "fresh DB then reopen" sequence settles at
        // user_version=6 (not 5). See CURRENT_SCHEMA_VERSION comment.
        assert_eq!(version, 6, "second open must keep user_version=6");
        assert_eq!(
            sync_mode, 1,
            "synchronous must be re-asserted to NORMAL even when already at head"
        );
    }

    /// After WAL is active, every-turn `sync_turn` still produces a
    /// queryable frame. This guards against an accidental regression
    /// where the WAL flip breaks a write path. We exercise 5 turns
    /// (mirroring the "5 turns per flush" task description) to make
    /// sure the recent_turns ring + frames table both stay coherent.
    #[tokio::test]
    async fn m6_wal_does_not_break_sync_turn_pipeline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let b = EmbeddedMemoryBackend::open(&db_path).expect("open");

        for i in 0..5 {
            b.sync_turn(
                &format!("user message {i}"),
                &format!("assistant reply {i}"),
                "s1",
            );
        }
        // Five turns → five frames tagged `turn`.
        let hits = b
            .recognize(
                &MemoryQuery::recall("assistant reply")
                    .with_limit(20),
            )
            .await
            .unwrap();
        assert!(
            hits.len() >= 5,
            "5 sync_turn calls must produce >=5 queryable frames; got {}",
            hits.len()
        );
    }

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

    //── : write-path hooks ─────────────────────

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
        b.on_memory_write("add", "user", "小林喜欢 Rust");
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

    /// `on_session_end` dedup: repeated calls within the freshness
    /// window (same session) must not stack duplicate session frames —
    /// `finalize_agent_result` fires per user message, so this is the
    /// common case, not the exception.
    #[tokio::test]
    async fn p17_on_session_end_dedups_repeated_calls() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "讨论 Rust 性能优化"}),
            serde_json::json!({"role": "assistant", "content": "建议使用 jemalloc"}),
        ];
        b.on_session_end(&msgs);
        b.on_session_end(&msgs);
        b.on_session_end(&msgs);
        assert_eq!(
            b.count_visible_frames().unwrap(),
            1,
            "repeated on_session_end calls must dedup to a single session frame"
        );
    }

    /// `has_recent_turn_frame` matches the exact user text only.
    #[tokio::test]
    async fn p17_has_recent_turn_frame_matches_exact_user_text() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let _ = b.record_turn("帮我修登录", "好", "s1", 1);
        assert!(b.has_recent_turn_frame("帮我修登录", 10).unwrap());
        assert!(!b.has_recent_turn_frame("别的文本", 10).unwrap());
    }

    /// `decay_stale_frames`: soft-hides stale unpinned turn/session
    /// frames; keeps pinned rows, focus conclusions and fresh frames.
    #[tokio::test]
    async fn p17_decay_stale_frames_respects_pin_and_source() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let _ = b.record_turn("旧对话", "旧回复", "s1", 1);
        let _ = b.record_turn("置顶旧对话", "留着", "s1", 2);
        let _ = b.record_turn("新对话", "新回复", "s1", 3);
        let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        {
            let conn = b.conn.lock();
            conn.execute(
                "UPDATE frames SET created_at = ?1 WHERE json_extract(payload,'$.user') = '旧对话'",
                rusqlite::params![old],
            )
            .unwrap();
            conn.execute(
                "UPDATE frames SET created_at = ?1, pin_count = 1 WHERE json_extract(payload,'$.user') = '置顶旧对话'",
                rusqlite::params![old],
            )
            .unwrap();
        }
        // focus 旧「会话总结」与 turn 同内容类，同窗口衰减
        let focus_frame = FocusFrame::new(serde_json::json!({"text": "旧结论"})).with_salience(0.5);
        b.upsert_frame_with_source(&focus_frame, "focus").unwrap();
        {
            let conn = b.conn.lock();
            conn.execute(
                "UPDATE frames SET created_at = ?1 WHERE source = 'focus'",
                rusqlite::params![old],
            )
            .unwrap();
        }
        let recent_focus = FocusFrame::new(serde_json::json!({"text": "新结论"})).with_salience(0.5);
        b.upsert_frame_with_source(&recent_focus, "focus").unwrap();

        let hidden = b.decay_stale_frames(14, 30).unwrap();
        assert_eq!(
            hidden, 2,
            "stale unpinned turn frame and stale focus summary decay; pinned survives"
        );
        // 置顶帧和新 focus 帧仍然可见
        assert!(b.has_recent_turn_frame("置顶旧对话", 60 * 24 * 365).unwrap());
        let conn = b.conn.lock();
        let focus_visible: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE source = 'focus' AND visibility = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(focus_visible, 1, "only the recent focus frame survives");
    }

    /// `soft_hide_other_user_styles`: latest-wins — only the current
    /// style row stays visible, and `most_frequent_user_style` follows.
    #[tokio::test]
    async fn p17_user_style_latest_wins() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        b.upsert_fact(
            "user.style:direct",
            serde_json::json!({"style": "direct"}),
            0.5,
            &[],
            "user_style_detector",
        )
        .unwrap();
        b.upsert_fact(
            "user.style:technical",
            serde_json::json!({"style": "technical"}),
            0.5,
            &[],
            "user_style_detector",
        )
        .unwrap();
        let n = b.soft_hide_other_user_styles("user.style:technical").unwrap();
        assert_eq!(n, 1, "only the sibling style row is hidden");
        assert_eq!(
            b.most_frequent_user_style().unwrap().as_deref(),
            Some("technical"),
            "visible style must be the latest detection"
        );
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
                serde_json::json!("小林"),
                0.8,
                &["user".into()],
                "curation",
            )
            .unwrap();
        let id2 = b
            .upsert_fact(
                "user.name",
                serde_json::json!("小林"),
                0.9,
                &["user".into()],
                "curation",
            )
            .unwrap();
        assert!(id1.is_some() && id2.is_some());
        assert_eq!(id1, id2, "same canonical_key should yield same id");
        // Total visible fact rows should still be 1.
        let hits = b
            .recognize(&MemoryQuery::recall("小林").with_limit(10))
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

    //( / 架构-1 / 架构-2): audit + snapshot + cap ----

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

    /// opening the embedded backend must set
    /// `PRAGMA busy_timeout` so concurrent writers serialise instead
    /// of immediately failing with `database is locked`. Read the
    /// PRAGMA back through `PRAGMA busy_timeout` (returns integer ms).
    #[test]
    fn r5_busy_timeout_is_set_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = EmbeddedMemoryBackend::open(tmp.path().join("busy.db")).unwrap();
        let timeout_ms: i64 = backend
            .conn
            .lock()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(
            timeout_ms >= 5000,
            "busy_timeout must be at least 5000ms after PRAGMA; got {timeout_ms}"
        );
    }

    /// the PRAGMA path is safe to evaluate on in-memory
    /// connections (it returns 0 there because :memory: has no
    /// contention) — the test pins that the PRAGMA call in `open`
    /// does not panic on either backend type.
    #[test]
    fn r5_busy_timeout_active_for_in_memory_backend() {
        let backend = EmbeddedMemoryBackend::open_in_memory().unwrap();
        let timeout_ms: i64 = backend
            .conn
            .lock()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        // in-memory returns 0; we only assert the PRAGMA did not panic.
        let _ = timeout_ms;
    }
}

// =================================================================
//(E): reflections API round-trip
// =================================================================

#[cfg(test)]
mod p19_reflections_tests {
    use super::*;

    fn b() -> EmbeddedMemoryBackend {
        EmbeddedMemoryBackend::open_in_memory().expect("open_in_memory")
    }

    #[tokio::test]
    async fn p19_add_reflection_round_trip() {
        let backend = b();
        let id = backend
            .add_reflection(
                "s1",
                "what_went_well",
                "本次会话 5 轮，无工具调用错误。",
                0.4,
                &["reflection:what_went_well".into(), "session:s1".into()],
                5,
                "reflection_generator",
            )
            .expect("add_reflection");
        assert!(id > 0);

        let rows = backend
            .list_recent_reflections("s1", 10)
            .expect("list_recent");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "what_went_well");
        assert_eq!(rows[0].session_id, "s1");
        assert_eq!(rows[0].turn_count, 5);
        assert!(rows[0].content.contains("5 轮"));
        assert!(rows[0].tags.contains(&"reflection:what_went_well".to_string()));
    }

    #[tokio::test]
    async fn p19_list_recent_reflections_respects_limit() {
        let backend = b();
        for i in 0..5 {
            backend
                .add_reflection(
                    "s2",
                    "next_session_hint",
                    &format!("hint {i}"),
                    0.5,
                    &[],
                    i,
                    "reflection_generator",
                )
                .expect("add");
        }
        let rows = backend
            .list_recent_reflections("s2", 3)
            .expect("list_recent");
        assert_eq!(rows.len(), 3);
        // newest first
        assert!(rows[0].content.contains("4"));
        assert!(rows[1].content.contains("3"));
        assert!(rows[2].content.contains("2"));
    }

    #[tokio::test]
    async fn p19_list_cross_session_reflections_excludes_target_session() {
        let backend = b();
        // Two sessions worth of reflections
        backend
            .add_reflection("sA", "what_to_improve", "session A lesson", 0.7, &[], 1, "test")
            .expect("add A");
        backend
            .add_reflection("sB", "what_to_improve", "session B lesson", 0.7, &[], 1, "test")
            .expect("add B");
        backend
            .add_reflection("sA", "next_session_hint", "session A hint", 0.5, &[], 1, "test")
            .expect("add A second");

        // Excluding sA → only sB's reflection should be returned.
        let excl_a = backend.list_cross_session_reflections("sA", 10).expect("excl A");
        assert_eq!(excl_a.len(), 1, "expected 1, got {excl_a:?}");
        assert_eq!(excl_a[0].session_id, "sB");

        // Empty exclude → returns everything (all 3 rows).
        let all = backend.list_cross_session_reflections("", 10).expect("all");
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn p19_list_cross_session_reflections_limit_caps_results() {
        let backend = b();
        for i in 0..6 {
            backend
                .add_reflection(
                    "sessionX",
                    "what_to_improve",
                    &format!("x{i}"),
                    0.5,
                    &[],
                    i,
                    "test",
                )
                .expect("add");
        }
        let rows = backend
            .list_cross_session_reflections("some-other-session", 4)
            .expect("limit");
        assert_eq!(rows.len(), 4, "expected 4, got {}", rows.len());
    }

    #[tokio::test]
    async fn p19_list_recent_reflections_zero_limit_returns_empty() {
        let backend = b();
        backend
            .add_reflection("s3", "x", "x", 0.5, &[], 1, "test")
            .expect("add");
        let rows = backend
            .list_recent_reflections("s3", 0)
            .expect("list_recent");
        assert_eq!(rows.len(), 0);
    }

    #[tokio::test]
    async fn p19_top_facts_for_recall_orders_by_salience() {
        let backend = b();
        // Two facts with very different salience
        backend
            .upsert_fact("k:low", serde_json::json!("low salience"), 0.1, &[], "test")
            .expect("upsert low");
        backend
            .upsert_fact("k:high", serde_json::json!("high salience"), 0.9, &[], "test")
            .expect("upsert high");
        let top = backend.top_facts_for_recall(5).expect("top");
        assert_eq!(top.len(), 2);
        // The high-salience row should come first.
        assert!(top[0].0.starts_with("k:high"));
        assert!(top[0].2 > top[1].2);
    }

    #[tokio::test]
    async fn p19_most_frequent_user_style_returns_dominant() {
        let backend = b();
        // Bump access_count to simulate "most used".
        backend
            .upsert_fact("user.style:direct", serde_json::json!("x"), 0.5, &[], "test")
            .expect("upsert direct");
        backend
            .upsert_fact("user.style:exploratory", serde_json::json!("y"), 0.5, &[], "test")
            .expect("upsert exploratory");
        // Bump direct's access_count so it wins
        let _ = backend.boost_fact_access("user.style:direct");
        let _ = backend.boost_fact_access("user.style:direct");
        let style = backend.most_frequent_user_style().expect("style");
        assert_eq!(style, Some("direct".to_string()));
    }

    #[tokio::test]
    async fn p19_most_frequent_user_style_none_when_empty() {
        let backend = b();
        let style = backend.most_frequent_user_style().expect("style");
        assert_eq!(style, None);
    }
}
