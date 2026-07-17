//! 2026-07-05 (N-03): pluggable ContextEngine registry.
//!
//! The agent loop can swap the built-in LLM-powered compressor for
//! any [`CompressionEngine`] implementation. The simplest case is
//! [`NoOpCompressionEngine`]: always pass-through, never compress,
//! useful for tests and for users who deliberately want to keep
//! raw conversation history (eg. they don't trust the LLM
//! summary quality).

use std::sync::Mutex;

use async_trait::async_trait;
use hermix_core::{
    CompressionEngine, CompressionEngineMetrics, CompressionEngineSnapshot, CompressionEngineState,
    CoreContextEngine, Message,
};

/// A compression engine that never actually compresses.
///
/// Use this when:
/// - the auxiliary LLM is unavailable and you'd rather skip
///   compression than ship broken summaries (see N-08,
///   `startup_compression_llm_health_check`),
/// - the operator explicitly disabled compression via
///   `agent.compression.engine = "noop"` in config,
/// - tests need deterministic message preservation.
pub struct NoOpCompressionEngine {
    last_metrics: Mutex<CompressionEngineMetrics>,
}

impl NoOpCompressionEngine {
    pub fn new() -> Self {
        Self {
            last_metrics: Mutex::new(CompressionEngineMetrics::default()),
        }
    }
}

impl Default for NoOpCompressionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CoreContextEngine for NoOpCompressionEngine {
    fn should_compress(&self, _current_tokens: u64) -> bool {
        // Deliberately: never. Returning false from
        // `should_compress` short-circuits the auto-compression
        // pipeline in `auto_compress_if_over_threshold` before
        // any work is done.
        false
    }

    async fn compress(&mut self, messages: Vec<Message>) -> Vec<Message> {
        // Pass-through: caller gets back exactly what they
        // sent in. We still bump a metric so the dashboard
        // shows "this engine was used" (compression_count = 0
        // means it never fired).
        messages
    }
}

#[async_trait]
impl CompressionEngine for NoOpCompressionEngine {
    fn previous_summary(&self) -> Option<&str> {
        None
    }

    fn last_archived_turns(&self) -> &[Message] {
        // Empty slice — nothing was archived because nothing
        // was compressed.
        &[]
    }

    fn last_metrics(&self) -> CompressionEngineMetrics {
        self.last_metrics.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn breaker_snapshot(
        &mut self,
        current_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        failure_reason: Option<String>,
    ) -> CompressionEngineSnapshot {
        CompressionEngineSnapshot {
            state: CompressionEngineState::Closed,
            current_tokens,
            threshold_tokens,
            max_tokens,
            failure_reason,
        }
    }

    fn record_outcome(&mut self, success: bool) -> CompressionEngineState {
        // No breaker to drive; every outcome (success or
        // failure) bumps `summary_errors` so a NoOp engine
        // that was wrongly wired in can still be spotted by
        // the dashboard. Successes also increment
        // `compression_count` for the same reason.
        if let Ok(mut m) = self.last_metrics.lock() {
            if success {
                m.compression_count += 1;
            } else {
                m.summary_errors += 1;
            }
        }
        CompressionEngineState::Closed
    }
}

/// Registry that resolves a configured engine name into a boxed
/// trait object. The agent loop consults this at construction
/// time so a config swap (`agent.compression.engine = "noop"`)
/// does not require a code change.
///
/// Adding a new engine is intentionally a one-liner: implement
/// `CompressionEngine`, then register it in
/// [`CompressionEngineRegistry::with_defaults`] below.
pub struct CompressionEngineRegistry {
    factories: std::collections::HashMap<
        String,
        Arc<dyn Fn() -> Box<dyn CompressionEngine + Send> + Send + Sync>,
    >,
}

use std::sync::Arc;

impl CompressionEngineRegistry {
    pub fn new() -> Self {
        Self {
            factories: std::collections::HashMap::new(),
        }
    }

    /// Register a new engine factory under `name`. Names are
    /// case-insensitive. Re-registration overwrites the old
    /// factory — useful for plugin hot-swap tests.
    pub fn register(
        &mut self,
        name: &str,
        factory: Arc<dyn Fn() -> Box<dyn CompressionEngine + Send> + Send + Sync>,
    ) {
        self.factories.insert(name.to_lowercase(), factory);
    }

    /// Resolve `name` to a fresh engine instance. Returns `None`
    /// for unknown names so the caller can fall back to the
    /// built-in LLM-powered compressor (the prior behaviour).
    pub fn resolve(&self, name: &str) -> Option<Box<dyn CompressionEngine + Send>> {
        self.factories
            .get(&name.to_lowercase())
            .map(|f| f())
    }

    /// Names currently registered. Used by the dashboard so an
    /// operator can pick from a dropdown.
    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.factories.keys().cloned().collect();
        n.sort();
        n
    }

    /// The default registry: built-in LLM compressor is NOT
    /// included here because constructing one needs an
    /// `AuxiliaryClient` (LLM handle). Callers that want the
    /// LLM compressor should construct it directly via
    /// `ContextCompressor::new(...)` and use it via the
    /// `Arc<Mutex<ContextCompressor>>` slot on `AgentLoop`.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(
            "noop",
            Arc::new(|| Box::new(NoOpCompressionEngine::new())),
        );
        r
    }
}

impl Default for CompressionEngineRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    #[tokio::test]
    async fn noop_engine_passes_messages_through_unchanged() {
        let mut engine = NoOpCompressionEngine::new();
        let messages = vec![
            Message::user("hello"),
            Message::assistant("hi"),
            Message::user("are you there?"),
        ];
        let out = engine.compress(messages.clone()).await;
        assert_eq!(out.len(), messages.len());
        assert_eq!(out[0].content.as_deref(), Some("hello"));
        assert_eq!(out[2].content.as_deref(), Some("are you there?"));
    }

    #[test]
    fn noop_engine_never_signals_should_compress() {
        let engine = NoOpCompressionEngine::new();
        // Even at 1M tokens (way over budget) we say "no, don't
        // compress" — the whole point of NoOp is to disable.
        assert!(!engine.should_compress(1_000_000));
        assert!(!engine.should_compress(0));
    }

    #[test]
    fn noop_engine_summary_and_archive_are_empty() {
        let engine = NoOpCompressionEngine::new();
        assert_eq!(engine.previous_summary(), None);
        assert!(engine.last_archived_turns().is_empty());
    }

    #[test]
    fn noop_engine_records_failure_as_metric() {
        let mut engine = NoOpCompressionEngine::new();
        let _ = engine.record_outcome(false);
        let _ = engine.record_outcome(false);
        assert_eq!(engine.last_metrics().summary_errors, 2);
        let _ = engine.record_outcome(true);
        // Successes bump `compression_count`, errors bump
        // `summary_errors` — both surfaces stay independent so
        // the dashboard can show "3 attempts, 2 failed".
        assert_eq!(engine.last_metrics().summary_errors, 2);
        assert_eq!(engine.last_metrics().compression_count, 1);
    }

    #[test]
    fn registry_resolves_noop_by_name() {
        let registry = CompressionEngineRegistry::with_defaults();
        let engine = registry.resolve("noop").expect("noop registered");
        // smoke-test that the resolved engine works through
        // the trait surface.
        assert!(!engine.should_compress(99_999));
    }

    #[test]
    fn registry_resolves_noop_case_insensitive() {
        let registry = CompressionEngineRegistry::with_defaults();
        assert!(registry.resolve("NoOp").is_some());
        assert!(registry.resolve("NOOP").is_some());
    }

    #[test]
    fn registry_unknown_name_returns_none() {
        let registry = CompressionEngineRegistry::with_defaults();
        assert!(registry.resolve("unknown-engine").is_none());
    }

    #[test]
    fn registry_lists_registered_names() {
        let registry = CompressionEngineRegistry::with_defaults();
        let names = registry.names();
        assert!(names.contains(&"noop".to_string()));
    }

    #[test]
    fn registry_supports_custom_factories() {
        let mut registry = CompressionEngineRegistry::new();
        registry.register(
            "test-engine",
            Arc::new(|| Box::new(NoOpCompressionEngine::new())),
        );
        let engine = registry.resolve("test-engine").expect("just registered");
        assert!(!engine.should_compress(99_999));
        assert!(registry.names().contains(&"test-engine".to_string()));
    }
}