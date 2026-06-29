//! MemoryRouter — fan out a query to N underlying `MemoryLayer`s,
//! deduplicate hits, re-rank by salience, and emit a single ordered
//! result list. Also fans out **write operations** to every backend,
//! replacing the legacy `MemoryProviderPlugin` trait.
//!
//! ## Write fanout contract
//!
//! Every write method iterates all backends, calls the corresponding
//! `MemoryLayer` hook, and tolerates per-backend errors according to
//! `config.tolerate_errors`. Backends that return `NotImplemented` or
//! whose implementation is a no-op are silently skipped.
//!
//! ## Thread safety
//!
//! Write methods are **synchronous** (not async) so that `agent_loop.rs`
//! can call them in hot paths without spawning extra tasks. Each backend
//! is responsible for its own internal synchronization (Mutex, channel,
//! etc.) if it needs to offload to another thread.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::memory_layer::{
    DynMemoryLayer, HealthStatus, MemoryError, MemoryHit, MemoryLayer, MemoryQuery, Result,
    SystemPromptBlock,
};
use crate::salience::{salience, SalienceConfig};
use crate::time_aware_recall::TimeAwareRecall;

/// A `MemoryLayer` that fans out to N backends, deduplicates by hit id
/// (keeping the highest-salience copy), and re-ranks by combined
/// salience.
pub struct MemoryRouter {
    backends: Vec<DynMemoryLayer>,
    /// Phase 17 (BaiLongma compression backfill): a parallel
    /// strong reference to the embedded SQLite backend if one was
    /// wired. The router keeps this separate from `backends` so
    /// callers can reach the concrete `EmbeddedMemoryBackend`
    /// type without going through the trait object. `None` when
    /// no embedded backend was added (e.g. a server that wires
    /// only remote HTTPS backends).
    embedded: Option<Arc<crate::embedded::EmbeddedMemoryBackend>>,
    config: RouterConfig,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Salience re-ranking config used at the router level.
    pub salience: SalienceConfig,
    /// Skip a backend if it returns an error (true) or propagate (false).
    pub tolerate_errors: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            salience: SalienceConfig::default(),
            tolerate_errors: true,
        }
    }
}

impl std::fmt::Debug for MemoryRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryRouter")
            .field("backends", &self.backends.iter().map(|b| b.name()).collect::<Vec<_>>())
            .field("config", &self.config)
            .finish()
    }
}

impl MemoryRouter {
    pub fn new(backends: Vec<DynMemoryLayer>) -> Self {
        // Phase 18 (P2-1) footgun guard: if the caller constructed a
        // router with zero backends *and* no concrete embedded
        // handle, every `sync_turn` / `on_memory_write` call will
        // silently no-op. That's almost always a wiring bug.
        // We log a warning instead of panicking so existing tests
        // (`empty_router_returns_empty`) keep working — but make
        // the bug visible at startup.
        if backends.is_empty() {
            tracing::warn!(
                target: "hermix.focus",
                "MemoryRouter::new() with empty backends — every write hook \
                 (sync_turn / on_memory_write / on_session_end) will silently \
                 no-op. If this is unintentional, register at least one backend \
                 via add_backend() or use new_with_embedded()."
            );
        }
        // Callers using `new()` directly don't get a concrete
        // embedded handle. They must use [`Self::new_with_embedded`]
        // if they need embedded-specific methods (compression
        // backfill, fact upsert) from outside the trait object.
        Self {
            backends,
            embedded: None,
            config: RouterConfig::default(),
        }
    }

    /// Phase 17 helper: build a router with a concrete pointer to
    /// the embedded backend. Use this when you need to call
    /// embedded-specific methods (compression backfill, fact
    /// upsert) from the agent loop.
    pub fn new_with_embedded(
        backends: Vec<DynMemoryLayer>,
        embedded: Arc<crate::embedded::EmbeddedMemoryBackend>,
    ) -> Self {
        // Phase 18 (P2-1): when backends list is empty, the trait-object
        // fan-out is dead but `find_embedded_backend()` still works.
        // That's actually OK for single-backend setups (write
        // hooks need at least one backend, but recognize still
        // works via the concrete Arc). Warn so it's visible.
        if backends.is_empty() {
            tracing::warn!(
                target: "hermix.focus",
                "MemoryRouter::new_with_embedded() with empty backends but a \
                 concrete embedded handle — write-hook fan-out will skip the \
                 embedded backend unless it is also pushed via backends.push()"
            );
        }
        Self {
            backends,
            embedded: Some(embedded),
            config: RouterConfig::default(),
        }
    }

    pub fn with_config(mut self, cfg: RouterConfig) -> Self {
        self.config = cfg;
        self
    }

    pub fn add_backend(&mut self, backend: DynMemoryLayer) {
        self.backends.push(backend);
    }

    /// Phase 17 (BaiLongma compression backfill): return a clone
    /// of the first embedded SQLite backend if the router has
    /// one. The agent loop uses this to call embedded-specific
    /// methods (`backfill_compression_fact`, `upsert_fact`)
    /// without going through the trait object.
    ///
    /// We clone the inner `Arc<EmbeddedMemoryBackend>` so we can
    /// hand out a concrete pointer. The backend was originally
    /// wrapped in `Arc::new(embedded) as DynMemoryLayer`; we keep
    /// a parallel `Arc<EmbeddedMemoryBackend>` in the router
    /// when this happens so callers can reach the concrete type.
    pub fn find_embedded_backend(&self) -> Option<Arc<crate::embedded::EmbeddedMemoryBackend>> {
        self.embedded.clone()
    }

    /// Phase 17 (safety net): bind every backend to the given
    /// session + hermix_home. Most call sites already call
    /// `initialize()` on each backend as they construct it (e.g.
    /// `memory_layer_adapter` does this for ByteRover and the
    /// embedded backend). This method exists so `MemoryRouter`
    /// can guarantee its backends are bound even if the
    /// construction site forgot. Safe to call multiple times —
    /// backends overwrite their own state.
    pub fn initialize_all(&self, session_id: &str, hermix_home: &str) {
        for backend in &self.backends {
            backend.initialize(session_id, hermix_home);
        }
    }

    pub fn backend_count(&self) -> usize {
        self.backends.len()
    }

    pub fn backend_names(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    /// Check health of all backends concurrently.
    pub async fn check_all_backends(&self) -> Vec<HealthStatus> {
        let mut handles = Vec::with_capacity(self.backends.len());
        for backend in &self.backends {
            let b = Arc::clone(backend);
            handles.push(tokio::spawn(async move { b.health_check().await }));
        }
        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(status) => results.push(status),
                Err(e) => results.push(HealthStatus {
                    backend: "unknown".to_string(),
                    available: false,
                    latency_ms: 0,
                    error: Some(format!("join error: {e}")),
                }),
            }
        }
        results
    }
}

#[async_trait]
impl MemoryLayer for MemoryRouter {
    fn name(&self) -> &str {
        "router"
    }

    fn is_available(&self) -> bool {
        self.backends.iter().any(|b| b.is_available())
    }

    async fn recognize(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>> {
        // 0. H4: if the caller didn't attach a time window, do it for
        //    them. This is the single chokepoint that makes "昨天" /
        //    "last week" queries filter by time across all 8 backends
        //    uniformly. Backends that already honour the window keep
        //    their behaviour; backends that ignore it are at worst
        //    doing extra work that's then dedup'd by the router.
        let mut query = query.clone();
        if query.time_window.is_none() {
            let recall = TimeAwareRecall::for_now();
            let hint = recall.analyze(&query.text);
            if let Some(w) = hint.window {
                query.time_window = Some(w);
                query.intent = hint.intent;
            }
        }
        // 1. Fan out concurrently to all backends.
        let mut handles = Vec::with_capacity(self.backends.len());
        for backend in &self.backends {
            if !backend.is_available() {
                continue;
            }
            let b = Arc::clone(backend);
            let q = query.clone();
            handles.push(tokio::spawn(async move { b.recognize(&q).await }));
        }
        // 2. Collect results; tolerate per-backend errors.
        let mut all_hits: Vec<MemoryHit> = Vec::new();
        for h in handles {
            match h.await {
                Ok(Ok(hits)) => all_hits.extend(hits),
                Ok(Err(e)) => {
                    if !self.config.tolerate_errors {
                        return Err(e);
                    }
                    tracing::warn!("memory backend error (tolerated): {e}");
                }
                Err(e) => {
                    if !self.config.tolerate_errors {
                        return Err(crate::memory_layer::MemoryError::Other(format!("join: {e}")));
                    }
                    tracing::warn!("backend task join error: {e}");
                }
            }
        }
        // 3. Deduplicate by hit id, keeping the highest-salience copy.
        let mut by_id: std::collections::HashMap<String, MemoryHit> =
            std::collections::HashMap::new();
        for h in all_hits {
            by_id
                .entry(h.id.clone())
                .and_modify(|existing| {
                    if h.salience > existing.salience {
                        *existing = h.clone();
                    }
                })
                .or_insert(h);
        }
        let mut deduped: Vec<MemoryHit> = by_id.into_values().collect();
        // 4. Re-rank by salience (router level) and truncate.
        let now = Utc::now();
        for hit in &mut deduped {
            let s = salience(hit.created_at, 0, 0, now, &self.config.salience);
            // Combine: take max(backend salience, router-recency salience).
            hit.salience = hit.salience.max(s.total);
        }
        deduped.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        deduped.truncate(query.limit);
        Ok(deduped)
    }

    async fn inject(&self, hits: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        // Group hits by source backend, ask each backend to format its
        // own block, then concatenate. Backends that have no `inject`
        // are skipped (an empty Vec is fine).
        let mut all_blocks: Vec<SystemPromptBlock> = Vec::new();
        for backend in &self.backends {
            let my_hits: Vec<&MemoryHit> = hits.iter().filter(|h| h.source == backend.name()).collect();
            if my_hits.is_empty() {
                continue;
            }
            let owned: Vec<MemoryHit> = my_hits.into_iter().cloned().collect();
            match backend.inject(&owned).await {
                Ok(blocks) => all_blocks.extend(blocks),
                Err(e) => {
                    if !self.config.tolerate_errors {
                        return Err(e);
                    }
                    tracing::warn!("backend inject error: {e}");
                }
            }
        }
        // Fallback if no backend produced a block: format a generic one.
        if all_blocks.is_empty() {
            let mut text = String::from("## Memory (router)\n\n");
            for h in hits {
                text.push_str(&format!("- [{} {:.2}] {}\n", h.source, h.salience, h.content));
            }
            all_blocks.push(SystemPromptBlock {
                text,
                priority: 50,
            });
        }
        Ok(all_blocks)
    }

    /// M1.6.4: fan out a focus conclusion to every backend that
    /// implements `upsert_conclusion`. Backends returning
    /// `NotImplemented` are silently skipped — they don't grow a
    /// write surface they don't need. Returns the number of
    /// backends that actually persisted the conclusion.
    ///
    /// Honours `config.tolerate_errors` — when false, a single
    /// backend failure surfaces as `MemoryError::Other` instead
    /// of being silently logged.
    async fn upsert_conclusion(&self, c: &crate::focus_stack::FocusConclusion) -> Result<()> {
        let mut persisted = 0usize;
        let mut last_err: Option<MemoryError> = None;
        for backend in &self.backends {
            match backend.upsert_conclusion(c).await {
                Ok(()) => persisted += 1,
                Err(MemoryError::NotImplemented(_)) => {
                    // Skip silently — this backend doesn't do writes.
                    continue;
                }
                Err(e) => {
                    if !self.config.tolerate_errors {
                        return Err(e);
                    }
                    tracing::debug!(
                        target: "hermix.focus",
                        backend = backend.name(),
                        error = %e,
                        "MemoryRouter::upsert_conclusion: backend failed (tolerated)"
                    );
                    last_err = Some(e);
                }
            }
        }
        if persisted == 0 {
            // No backend supported writes — treat as no-op success.
            tracing::debug!(
                target: "hermix.focus",
                conclusion_id = %c.source_frame_id,
                "MemoryRouter::upsert_conclusion: no backend implemented write"
            );
            return Ok(());
        }
        if let Some(e) = last_err {
            // Some backends wrote, some failed tolerated. Log once.
            tracing::debug!(
                target: "hermix.focus",
                persisted,
                conclusion_id = %c.source_frame_id,
                error = %e,
                "MemoryRouter::upsert_conclusion: partial success"
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Write-path fanout — overrides of the trait's default no-op hooks.
    // These are trait method overrides, NOT inherent methods, so:
    //   - no `pub` qualifier (trait items share the trait's visibility)
    //   - bodies iterate `self.backends` and dispatch to each backend's
    //     own implementation. The trait dispatch is automatic — Rust
    //     calls *our* fan-out version when an external caller invokes
    //     `router.sync_turn(...)` and then we forward to every backend.
    // ------------------------------------------------------------------

    /// Fan-out `sync_turn` to all backends. Synchronous — each backend
    /// is responsible for its own threading if needed (ByteRover uses
    /// `std::thread::spawn` internally).
    fn sync_turn(&self, user: &str, assistant: &str, session_id: &str) {
        for backend in &self.backends {
            backend.sync_turn(user, assistant, session_id);
        }
    }

    /// Fan-out `on_memory_write` to all backends.
    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        for backend in &self.backends {
            backend.on_memory_write(action, target, content);
        }
    }

    /// Fan-out `on_turn_start` to all backends.
    fn on_turn_start(&self, turn_number: u32, message: &str) {
        for backend in &self.backends {
            backend.on_turn_start(turn_number, message);
        }
    }

    /// Fan-out `on_session_end` to all backends.
    fn on_session_end(&self, messages: &[serde_json::Value]) {
        for backend in &self.backends {
            backend.on_session_end(messages);
        }
    }

    /// Fan-out `on_pre_compress`, concatenate non-empty results.
    /// Returns the concatenated string (or empty string if no backend
    /// produced a note).
    fn on_pre_compress(&self, messages: &[serde_json::Value]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for backend in &self.backends {
            let note = backend.on_pre_compress(messages);
            if !note.is_empty() {
                parts.push(note);
            }
        }
        parts.join("\n")
    }

    /// Fan-out `on_delegation` to all backends.
    fn on_delegation(&self, task: &str, sub_agent_id: &str) {
        for backend in &self.backends {
            backend.on_delegation(task, sub_agent_id);
        }
    }
}

// ----------------------------------------------------------------------
// Inherent methods on MemoryRouter — not part of the trait.
// `collect_system_prompt_blocks` lives here because no equivalent
// method exists on the `MemoryLayer` trait (the trait exposes only
// per-backend `system_prompt_block`); this helper aggregates them.
// ----------------------------------------------------------------------

impl MemoryRouter {
    /// Collect all backends' `system_prompt_block` contributions and
    /// concatenate them.
    pub fn collect_system_prompt_blocks(&self) -> String {
        let mut block = String::new();
        for backend in &self.backends {
            let chunk = backend.system_prompt_block();
            if !chunk.is_empty() {
                block.push_str(&chunk);
                block.push('\n');
            }
        }
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_layer::{MemoryHit, MemoryQuery};
    use async_trait::async_trait;
    use chrono::Utc;

    /// Stub backend returning a fixed set of hits.
    struct StubBackend {
        name: String,
        hits: Vec<MemoryHit>,
    }

    #[async_trait]
    impl MemoryLayer for StubBackend {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        async fn recognize(&self, _: &MemoryQuery) -> Result<Vec<MemoryHit>> {
            Ok(self.hits.clone())
        }
        async fn inject(&self, _: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
            Ok(Vec::new())
        }
    }

    fn make_hit(id: &str, salience: f32) -> MemoryHit {
        MemoryHit {
            id: id.to_string(),
            content: format!("content-{id}"),
            salience,
            created_at: Utc::now(),
            metadata: serde_json::json!({}),
            source: "stub".to_string(),
        }
    }

    #[tokio::test]
    async fn empty_router_returns_empty() {
        let r = MemoryRouter::new(Vec::new());
        let q = MemoryQuery::recall("hi");
        let hits = r.recognize(&q).await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn fan_out_merges_hits() {
        let b1 = Arc::new(StubBackend {
            name: "a".into(),
            hits: vec![make_hit("1", 0.9), make_hit("2", 0.5)],
        });
        let b2 = Arc::new(StubBackend {
            name: "b".into(),
            hits: vec![make_hit("3", 0.7), make_hit("4", 0.3)],
        });
        let r = MemoryRouter::new(vec![b1, b2]);
        let q = MemoryQuery::recall("hi").with_limit(10);
        let hits = r.recognize(&q).await.unwrap();
        assert_eq!(hits.len(), 4);
        // Top hit should be id 1 (salience 0.9).
        assert_eq!(hits[0].id, "1");
    }

    #[tokio::test]
    async fn deduplicate_keeps_highest_salience() {
        let b1 = Arc::new(StubBackend {
            name: "a".into(),
            hits: vec![make_hit("1", 0.5)],
        });
        let b2 = Arc::new(StubBackend {
            name: "b".into(),
            hits: vec![make_hit("1", 0.9), make_hit("1", 0.7)], // same id, diff salience
        });
        let r = MemoryRouter::new(vec![b1, b2]);
        let q = MemoryQuery::recall("hi");
        let hits = r.recognize(&q).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!((hits[0].salience - 0.9).abs() < 1e-3);
    }

    #[tokio::test]
    async fn respects_query_limit() {
        let b = Arc::new(StubBackend {
            name: "a".into(),
            hits: (0..20).map(|i| make_hit(&format!("{i}"), 1.0 - i as f32 * 0.01)).collect(),
        });
        let r = MemoryRouter::new(vec![b]);
        let q = MemoryQuery::recall("hi").with_limit(5);
        let hits = r.recognize(&q).await.unwrap();
        assert_eq!(hits.len(), 5);
    }

    #[tokio::test]
    async fn unavailable_backends_are_skipped() {
        struct Unavailable;
        #[async_trait]
        impl MemoryLayer for Unavailable {
            fn name(&self) -> &str { "off" }
            fn is_available(&self) -> bool { false }
            async fn recognize(&self, _: &MemoryQuery) -> Result<Vec<MemoryHit>> { unreachable!() }
            async fn inject(&self, _: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> { unreachable!() }
        }
        let b = Arc::new(StubBackend {
            name: "ok".into(),
            hits: vec![make_hit("1", 0.5)],
        });
        let r = MemoryRouter::new(vec![Arc::new(Unavailable), b]);
        let q = MemoryQuery::recall("hi");
        let hits = r.recognize(&q).await.unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn backend_count_and_names() {
        let b1: Arc<dyn MemoryLayer> = Arc::new(StubBackend { name: "a".into(), hits: vec![] });
        let b2: Arc<dyn MemoryLayer> = Arc::new(StubBackend { name: "b".into(), hits: vec![] });
        let r = MemoryRouter::new(vec![b1, b2]);
        assert_eq!(r.backend_count(), 2);
        assert_eq!(r.backend_names(), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn inject_with_no_backend_blocks_falls_back() {
        struct NoInject;
        #[async_trait]
        impl MemoryLayer for NoInject {
            fn name(&self) -> &str { "x" }
            fn is_available(&self) -> bool { true }
            async fn recognize(&self, _: &MemoryQuery) -> Result<Vec<MemoryHit>> { Ok(vec![]) }
            async fn inject(&self, _: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> { Ok(vec![]) }
        }
        let r = MemoryRouter::new(vec![Arc::new(NoInject)]);
        let hits = vec![make_hit("1", 0.5)];
        let blocks = r.inject(&hits).await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("Memory"));
    }

    /// H4: a router-level test that the auto-time-window path runs
    /// when the caller doesn't set one. Backend captures the
    /// `MemoryQuery` it received so we can assert the window was
    /// attached.
    #[tokio::test]
    async fn router_attaches_time_window_from_query_text() {
        use crate::memory_layer::TimeWindow;
        use std::sync::Mutex;

        struct Capture {
            seen: Mutex<Option<MemoryQuery>>,
        }
        #[async_trait]
        impl MemoryLayer for Capture {
            fn name(&self) -> &str { "capture" }
            fn is_available(&self) -> bool { true }
            async fn recognize(&self, q: &MemoryQuery) -> Result<Vec<MemoryHit>> {
                *self.seen.lock().unwrap() = Some(q.clone());
                Ok(vec![])
            }
            async fn inject(&self, _: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
                Ok(vec![])
            }
        }

        let cap = Arc::new(Capture { seen: Mutex::new(None) });
        let r = MemoryRouter::new(vec![cap.clone()]);
        // No time_window set on the query — router should derive one
        // from the text "昨天" via TimeAwareRecall.
        let q = MemoryQuery::recall("昨天我们聊了什么");
        let _ = r.recognize(&q).await.unwrap();
        let seen = cap.seen.lock().unwrap().clone().expect("backend called");
        let tw: TimeWindow = seen.time_window.expect("time window auto-attached");
        // Sanity: the window should cover some 24h-48h-ago range.
        let now = Utc::now();
        let diff = now - tw.end.unwrap();
        assert!(
            diff.num_hours() >= 23 && diff.num_hours() <= 25,
            "end should be ~24h ago, got {diff:?}"
        );
    }

    /// H4: when the caller *already* set a time window, the router
    /// must respect it (don't overwrite). Without this guard a
    /// caller that picked a non-default window would be surprised
    /// when "昨天" in the query text overrides their explicit choice.
    #[tokio::test]
    async fn router_respects_explicit_time_window() {
        use crate::memory_layer::TimeWindow;
        use chrono::Duration;
        use std::sync::Mutex;

        struct Capture {
            seen: Mutex<Option<MemoryQuery>>,
        }
        #[async_trait]
        impl MemoryLayer for Capture {
            fn name(&self) -> &str { "capture" }
            fn is_available(&self) -> bool { true }
            async fn recognize(&self, q: &MemoryQuery) -> Result<Vec<MemoryHit>> {
                *self.seen.lock().unwrap() = Some(q.clone());
                Ok(vec![])
            }
            async fn inject(&self, _: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>> {
                Ok(vec![])
            }
        }

        let cap = Arc::new(Capture { seen: Mutex::new(None) });
        let r = MemoryRouter::new(vec![cap.clone()]);
        let now = Utc::now();
        let explicit = TimeWindow {
            start: now - Duration::days(90),
            end: Some(now),
        };
        let q = MemoryQuery::recall("yesterday please").with_time_window(explicit.clone());
        let _ = r.recognize(&q).await.unwrap();
        let seen = cap.seen.lock().unwrap().clone().expect("backend called");
        assert_eq!(seen.time_window, Some(explicit));
    }

    #[tokio::test]
    async fn check_all_backends_returns_status() {
        let b1: Arc<dyn MemoryLayer> = Arc::new(StubBackend {
            name: "a".into(),
            hits: vec![],
        });
        let b2: Arc<dyn MemoryLayer> = Arc::new(StubBackend {
            name: "b".into(),
            hits: vec![],
        });
        let r = MemoryRouter::new(vec![b1, b2]);
        let results = r.check_all_backends().await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|s| s.available));
    }
}
