//! Integration tests for the compression pipeline entry point.
//!
//! Verifies threshold gating and NoOp engine pass-through.
//! Moved from `src/compression_ops.rs` integration_tests module (N-10).

use std::sync::Arc;
use std::sync::Mutex;

use hermix_agent::compression_ops::auto_compress_if_over_threshold;
use hermix_agent::{AgentCallbacks, AgentConfig, ContextManager};
use hermix_agent::no_op_compression_engine::NoOpCompressionEngine;
use hermix_core::CompressionEngine;
use tokio::sync::Mutex as TokioMutex;

/// Below-threshold conversation must NOT trigger any
/// compression work, regardless of which engine is wired.
/// (NoOp would no-op anyway, but the threshold gate is
/// upstream of the engine call.)
#[tokio::test]
async fn below_threshold_skips_compression_entirely() {
    let cfg = AgentConfig::default();
    let callbacks = AgentCallbacks::default();
    let last_archive = Mutex::new(Vec::new());
    let latest_summary = Mutex::new(None);
    let latest_archived = Mutex::new(None);
    let latest_metrics = Mutex::new(None);
    let mut ctx = ContextManager::new(100_000).with_output_reserve(0);
    for i in 0..3 {
        ctx.add_message(hermix_core::Message::user(format!("hi {i}")));
    }
    // No engine at all.
    auto_compress_if_over_threshold(
        &cfg,
        &callbacks,
        None,
        &last_archive,
        &latest_summary,
        &latest_archived,
        &latest_metrics,
        &mut ctx,
        |_| None,
    )
    .await;
    assert_eq!(latest_summary.lock().unwrap().clone(), None);
    assert_eq!(latest_archived.lock().unwrap().clone(), None);
    // Messages unchanged.
    assert_eq!(ctx.get_messages().len(), 3);
}

/// Above-threshold + NoOp engine: engine says "don't
/// compress", archive should still happen, but the
/// compression summary stays `None` and the message list
/// is untouched (NoOp passes through).
#[tokio::test]
async fn above_threshold_with_noop_engine_archives_but_does_not_compress() {
    let cfg = AgentConfig::default();
    let callbacks = AgentCallbacks::default();
    let last_archive = Mutex::new(Vec::new());
    let latest_summary = Mutex::new(None);
    let latest_archived = Mutex::new(None);
    let latest_metrics = Mutex::new(None);
    // Small budget so 5 user msgs blow past the threshold.
    let mut ctx = ContextManager::new(40).with_output_reserve(0);
    for i in 0..5 {
        ctx.add_message(hermix_core::Message::user(format!(
            "user message {} {}",
            i,
            "x".repeat(60)
        )));
    }
    let n_before = ctx.get_messages().len();
    let engine: Arc<TokioMutex<dyn CompressionEngine + Send>> =
        Arc::new(TokioMutex::new(NoOpCompressionEngine::new()));
    auto_compress_if_over_threshold(
        &cfg,
        &callbacks,
        Some(&engine),
        &last_archive,
        &latest_summary,
        &latest_archived,
        &latest_metrics,
        &mut ctx,
        |_| None,
    )
    .await;
    // NoOp shouldn't have produced a summary.
    assert_eq!(
        latest_summary.lock().unwrap().clone(),
        None,
        "NoOp engine must not produce a summary"
    );
    // NoOp must not archive either (it's a pass-through).
    assert!(
        last_archive.lock().unwrap().is_empty(),
        "NoOp engine must not write archive refs"
    );
    // Message list unchanged.
    assert_eq!(ctx.get_messages().len(), n_before);
}
