//! Unified `MemoryLayer` trait — the single interface all memory backends implement.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3 H5.
//!
//! New backends — embedded SQLite, vector DBs, plain files —
//! can be plugged in by implementing these 4 methods. The agent loop
//! speaks only `MemoryLayer`; it doesn't care which backend is behind
//! the trait object.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::focus_stack::FocusConclusion;

/// A time window used by both [`MemoryQuery::time_window`] and
/// [`TimeAwareRecall`]. `end` may be `None` to mean "until now".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}

impl TimeWindow {
    pub fn last_hours(hours: i64) -> Self {
        let end = Utc::now();
        let start = end - chrono::Duration::hours(hours);
        Self {
            start,
            end: Some(end),
        }
    }

    pub fn last_days(days: i64) -> Self {
        Self::last_hours(days * 24)
    }

    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        // Allow 1 second of grace at the upper boundary so callers that
        // pass `Utc::now()` *after* the window was built (and thus a few
        // microseconds newer than `end`) are still considered in-window.
        let end_with_grace = self.end.map(|e| e + chrono::Duration::seconds(1));
        t >= self.start && end_with_grace.map(|e| t <= e).unwrap_or(true)
    }
}

/// The kind of intent the agent is trying to satisfy. Backends may use
/// this to bias which memories to return (e.g. a "recall" intent triggers
/// FTS, a "summarize" intent triggers conclusion frames only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Intent {
    /// "What did I/we do X?" — surface relevant past frames
    Recall,
    /// "Summarize session/conversation" — prefer focus conclusions
    Summarize,
    /// "Continue from where we left off" — most recent frames
    Resume,
    /// Free-form — backend decides
    Other(String),
}

/// A memory query — the input to [`MemoryLayer::recognize`].
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    pub intent: Intent,
    pub text: String,
    pub time_window: Option<TimeWindow>,
    pub limit: usize,
    /// L1: optional query embedding. When set, backends that support
    /// vector search (`cosine_topk`) include a cosine-recall leg in
    /// their `recognize` result. Backends without vector support
    /// silently ignore this field.
    /// /// Carried as `Vec<f32>` (not `Arc<[f32]>`) because:
    /// 1. MemoryQuery is cloned per-query in agent loops; Vec is
    /// cheap to clone for typical 384-1024-dim embeddings.
    /// 2. The router constructs the embedding once and passes the
    /// same Vec into multiple backends' recognize calls.
    pub embedding: Option<Vec<f32>>,
    /// Phase 18 (session isolation): when set, backends scope recall
    /// to entries whose `source` matches this session id. `None`
    /// means global recall (all sessions) — the legacy behaviour.
    pub session_id: Option<String>,
}

impl MemoryQuery {
    pub fn recall(text: impl Into<String>) -> Self {
        Self {
            intent: Intent::Recall,
            text: text.into(),
            time_window: None,
            limit: 10,
            embedding: None,
            session_id: None,
        }
    }

    pub fn with_limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }

    pub fn with_time_window(mut self, w: TimeWindow) -> Self {
        self.time_window = Some(w);
        self
    }

    pub fn with_intent(mut self, intent: Intent) -> Self {
        self.intent = intent;
        self
    }

    /// Scope recall to entries from a specific session.
    pub fn with_session_id(mut self, sid: String) -> Self {
        self.session_id = Some(sid);
        self
    }

    /// L1: attach a query embedding (e.g. produced by
    /// `hermix-intelligence::EmbeddingProvider`). Triggers vector
    /// recall on backends that support it.
    pub fn with_embedding(mut self, e: Vec<f32>) -> Self {
        self.embedding = Some(e);
        self
    }
}

/// A single memory hit returned by [`MemoryLayer::recognize`].
/// /// Salience is **assigned by the backend**, not by the router. The router
/// may re-rank by combining backend salience with router-level signals
/// (recency, frequency), but the backend is the source of truth for
/// "how relevant does this backend think this is".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    /// Stable id (backend-defined: rowid, uuid, etc.)
    pub id: String,
    /// The memory content
    pub content: String,
    /// Backend-assigned salience in [0.0, 1.0]
    pub salience: f32,
    /// When this memory was created (UTC)
    pub created_at: DateTime<Utc>,
    /// Free-form metadata (tags, source, etc.)
    pub metadata: serde_json::Value,
    /// Which backend produced this hit
    pub source: String,
}

/// A block of text to be appended to the system prompt. Backends may
/// produce 0 or 1 of these per query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPromptBlock {
    pub text: String,
    pub priority: i32, // higher = more important
}

/// Unified prompt formatter for memory hits. Ensures consistent format
/// and token budget across all backends.
pub trait SystemPromptFormatter: Send + Sync {
    /// Format memory hits into system prompt blocks with a token budget.
    fn format(&self, hits: &[MemoryHit], budget: usize) -> Vec<SystemPromptBlock>;
}

/// Default formatter: markdown list with source and salience, truncated to budget.
pub struct DefaultPromptFormatter;

impl SystemPromptFormatter for DefaultPromptFormatter {
    fn format(&self, hits: &[MemoryHit], budget: usize) -> Vec<SystemPromptBlock> {
        if hits.is_empty() {
            return Vec::new();
        }
        let mut text = String::from("## Memory\n\n");
        let mut remaining = budget.saturating_sub(text.len());
        for h in hits {
            let line = format!("- [{} {:.2}] {}\n", h.source, h.salience, h.content);
            if remaining < line.len() {
                text.push_str(&format!("- ... ({} more hits truncated)\n", hits.len()));
                break;
            }
            text.push_str(&line);
            remaining -= line.len();
        }
        vec![SystemPromptBlock {
            text,
            priority: 50,
        }]
    }
}

#[derive(Debug, Error, Serialize, Deserialize)]
pub enum MemoryError {
    #[error("backend not available: {0}")]
    Unavailable(String),
    #[error("io: {0}")]
    #[serde(skip_serializing, skip_deserializing)]
    Io(#[from] std::io::Error),
    #[error("sql: {0}")]
    #[serde(skip_serializing, skip_deserializing)]
    Sql(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    #[serde(skip_serializing, skip_deserializing)]
    Serde(#[from] serde_json::Error),
    #[error("not implemented: {0}")]
    NotImplemented(String),
    #[error("other: {0}")]
    Other(String),
}

impl From<&str> for MemoryError {
    fn from(s: &str) -> Self {
        MemoryError::Other(s.to_string())
    }
}

impl From<String> for MemoryError {
    fn from(s: String) -> Self {
        MemoryError::Other(s)
    }
}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// The unified memory interface. All 8 external plugins + the embedded
/// backend adapt to this.
/// /// Methods are async because the 8 external backends are HTTPS APIs.
/// The embedded backend is sync internally but exposes the async signature
/// for trait uniformity.
#[async_trait]
pub trait MemoryLayer: Send + Sync {
    /// Stable name (e.g. "embedded-sqlite", "hindsight", "mem0").
    fn name(&self) -> &str;

    /// Whether the backend is configured and ready. Cheap to call.
    fn is_available(&self) -> bool;

    /// Return relevant memory hits for a query. The backend decides
    /// ranking and salience; the caller applies final re-ranking if needed.
    async fn recognize(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>>;

    /// Build a system-prompt block from the recognized hits. Backends
    /// with no prompt-block concept may return an empty Vec.
    async fn inject(&self, hits: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>>;

    /// M1.6.4: persist a `FocusConclusion` from the focus stack.
    /// Default returns `MemoryError::NotImplemented` so the 9
    /// external HTTPS plugins don't have to grow a write surface
    /// they don't actually need. Only the embedded SQLite
    /// backend overrides this to call `upsert_conclusion`.
    /// /// The router fans out every conclusion to every backend;
    /// backends that don't implement this are skipped silently
    /// (the `NotImplemented` error is filtered, not surfaced).
    async fn upsert_conclusion(&self, _c: &FocusConclusion) -> Result<()> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support upsert_conclusion",
            self.name()
        )))
    }

    /// L1: persist (or replace) the embedding for a previously-
    /// inserted frame. `id` is the frame id (`MemoryHit.id` or
    /// `FocusFrame.id.to_string()`). `embedding` is the raw
    /// f32 vector; `dim` and `model` are recorded alongside so
    /// cosine search can skip rows that came from a different
    /// embedding model (cosine is only meaningful between
    /// vectors of the same dim and ideally the same model).
    /// /// Default returns `NotImplemented`. Only the embedded
    /// SQLite backend overrides this in L1.3.
    async fn upsert_embedding(
        &self,
        _id: &str,
        _embedding: &[f32],
        _model: &str,
    ) -> Result<()> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support vector storage",
            self.name()
        )))
    }

    /// L1: top-k nearest neighbours by cosine similarity. Skips
    /// rows whose stored dim != `query.len()` or whose model
    /// doesn't match the provided `model` (pass an empty string
    /// to skip the model check, useful for cross-model cosine
    /// in tests). `min_score` filters low-similarity hits.
    /// /// Default returns `NotImplemented`. Only the embedded
    /// SQLite backend overrides this in L1.3.
    /// /// O(N) scan: fine for the L1.5 target corpus of "30+ tools
    /// → top-5". When the corpus grows to millions of rows we
    /// graduate to `usearch` HNSW in a later phase.
    async fn cosine_topk(
        &self,
        _query: &[f32],
        _k: usize,
        _min_score: f32,
        _model: &str,
    ) -> Result<Vec<MemoryHit>> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support vector search",
            self.name()
        )))
    }

    /// Save a key-value pair to the backend. Default returns NotImplemented.
    async fn save(&self, _key: &str, _value: &str, _metadata: serde_json::Value) -> Result<()> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support save",
            self.name()
        )))
    }

    /// Update an existing entry by id. Default returns NotImplemented.
    async fn update(&self, _id: &str, _value: &str, _metadata: serde_json::Value) -> Result<()> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support update",
            self.name()
        )))
    }

    /// Delete an entry by id. Default returns NotImplemented.
    async fn delete(&self, _id: &str) -> Result<()> {
        Err(MemoryError::NotImplemented(format!(
            "{} does not support delete",
            self.name()
        )))
    }

    /// Convenience: `recognize` then `inject`, returning the top hits
    /// alongside the prompt block.
    async fn recall(&self, query: &MemoryQuery) -> Result<(Vec<MemoryHit>, Vec<SystemPromptBlock>)> {
        let hits = self.recognize(query).await?;
        let blocks = self.inject(&hits).await?;
        Ok((hits, blocks))
    }

    /// Health check for the backend. Returns availability status and latency.
    /// Default implementation returns available with 0 latency.
    async fn health_check(&self) -> HealthStatus {
        HealthStatus {
            backend: self.name().to_string(),
            available: self.is_available(),
            latency_ms: 0,
            error: None,
        }
    }

    // ---------------------------------------------------------------
    // Write-path hooks — unified from the legacy `MemoryProviderPlugin`
    // trait (Phase 17). Every backend defaults to a no-op so the 8
    // external HTTPS plugins don't have to grow a write surface they
    // don't actually need. The agent loop fans out to every registered
    // `MemoryLayer` via `PluginManager::memory_providers`.
    // ---------------------------------------------------------------

    /// Bind the backend to a session + hermix_home. Backends that
    /// need to create workspace dirs or open a connection
    /// pool override this. Default is a no-op.
    fn initialize(&self, _session_id: &str, _hermix_home: &str) {}

    /// Clean shutdown — flush buffers, close pools. Default no-op.
    fn shutdown(&self) {}

    /// Persist a completed user→assistant turn. Default no-op.
    fn sync_turn(&self, _user: &str, _assistant: &str, _session_id: &str) {}

    /// Notify the backend that the user issued a `memory` tool call
    /// (`action` ∈ {add, replace, remove}, `target` is the section
    /// name, `content` is the new/old text). Default no-op.
    fn on_memory_write(&self, _action: &str, _target: &str, _content: &str) {}

    /// Turn-start notification. Default no-op.
    fn on_turn_start(&self, _turn_number: u32, _message: &str) {}

    /// Session-end notification. Default no-op.
    fn on_session_end(&self, _messages: &[serde_json::Value]) {}

    /// Pre-compression hook — backend returns a short note that
    /// the compressor concatenates into the preserved memory.
    /// Default returns an empty string.
    fn on_pre_compress(&self, _messages: &[serde_json::Value]) -> String {
        String::new()
    }

    /// Static text injected into the system prompt. Backends with no
    /// prompt-block concept return an empty string (default).
    fn system_prompt_block(&self) -> String {
        String::new()
    }

    /// Sub-agent delegation hook. Called when a child agent is
    /// spawned via the `delegate_task` tool. Default no-op.
    /// Moved from the legacy `MemoryProviderPlugin::on_delegation`
    /// in T1-A — no plugin ever overrode it, but the agent loop
    /// iterates every provider on every delegation, so the trait
    /// method must exist on `MemoryLayer`.
    fn on_delegation(&self, _task: &str, _sub_agent_id: &str) {}
}

/// Health status of a memory backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub backend: String,
    pub available: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// Re-exported convenience type for trait objects.
pub type DynMemoryLayer = Arc<dyn MemoryLayer>;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_hit(id: &str, content: &str, salience: f32) -> MemoryHit {
        MemoryHit {
            id: id.to_string(),
            content: content.to_string(),
            salience,
            created_at: Utc::now(),
            metadata: serde_json::json!({}),
            source: "test".to_string(),
        }
    }

    #[test]
    fn default_formatter_empty_hits() {
        let fmt = DefaultPromptFormatter;
        let blocks = fmt.format(&[], 1000);
        assert!(blocks.is_empty());
    }

    #[test]
    fn default_formatter_single_hit() {
        let fmt = DefaultPromptFormatter;
        let hits = vec![make_hit("1", "hello world", 0.8)];
        let blocks = fmt.format(&hits, 1000);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("hello world"));
        assert!(blocks[0].text.contains("0.80"));
    }

    #[test]
    fn default_formatter_truncates_at_budget() {
        let fmt = DefaultPromptFormatter;
        let hits: Vec<MemoryHit> = (0..100)
            .map(|i| make_hit(&i.to_string(), &format!("content-{}", i), 0.5))
            .collect();
        let blocks = fmt.format(&hits, 200); // very small budget
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("truncated"));
    }
}
