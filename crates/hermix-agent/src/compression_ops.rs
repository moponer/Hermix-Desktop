//! Auto-compression pipeline used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No
//! behavior change** — pure relocation.
//!
//! Items living here:
//! - `auto_compress_if_over_threshold` — fires the auto-compression
//!   pipeline when context pressure crosses the threshold. Archives
//!   the middle of the conversation to disk, calls the LLM compressor
//!   if one is wired, and updates the live compression-state slots
//!   (`latest_compressed_summary` / `archived_turns` / `metrics`)
//!   so `local_agent_service` can persist them in the same DB
//!   transaction that updates `sessions.compressed_summary`.

use std::sync::{Arc, Mutex};

use hermix_core::Message;
use tokio::sync::Mutex as TokioMutex;

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::archive::{archive_context_section, render_archive_summary, ArchiveRef};
use crate::compression::{CompressionMetrics, ContextCompressor};
use crate::context::ContextManager;
use crate::message_helpers::{emit_compaction_status, emit_status};

/// Auto-compress when context pressure crosses the configured
/// threshold. The pipeline:
///
/// 1. Compute pressure and emit a `lifecycle` status callback.
/// 2. Pre-attempt breaker snapshot for the `CompactionStatusBanner`.
/// 3. Archive the middle of the conversation to disk (best-effort).
/// 4. Inject the archive summary into the context so the LLM knows
///    where to "look up" older context if asked.
/// 5. Inject the `MemoryRouter::on_pre_compress` note (best-effort).
/// 6. Run the LLM compressor (if wired) and update the live
///    compression-state slots; otherwise fall back to the simple
///    character-based `ContextManager::compress`.
///
/// The compression-state slots (`latest_compressed_summary` / etc)
/// are read by `finalize_agent_result` and shipped as the
/// `compressed_summary` / `compression_archived_turns` /
/// `compression_metrics` fields on `AgentResult`. `local_agent_service`
/// then persists them in the same DB transaction that updates
/// `sessions.compressed_summary` (without this capture, the
/// in-memory conversation would be the only place that knew what
/// got compressed — `flush_messages` would wipe those rows before
/// the loop returns to the caller).
#[allow(clippy::too_many_arguments)]
pub async fn auto_compress_if_over_threshold<F>(
    config: &AgentConfig,
    callbacks: &AgentCallbacks,
    compressor: Option<&Arc<TokioMutex<ContextCompressor>>>,
    last_archive_refs: &Mutex<Vec<ArchiveRef>>,
    latest_compressed_summary: &Mutex<Option<String>>,
    latest_compression_archived_turns: &Mutex<Option<Vec<Message>>>,
    latest_compression_metrics: &Mutex<Option<CompressionMetrics>>,
    ctx: &mut ContextManager,
    memory_pre_compress_note_fn: F,
) where
    F: FnOnce(&[Message]) -> Option<String>,
{
    let total_tokens = ctx.total_tokens();
    let max_c = ctx.max_context_tokens().max(1);
    let threshold = ctx.pressure_threshold_tokens();
    if total_tokens <= threshold {
        return;
    }
    let message = format!(
        "Context pressure at {}%, triggering compression",
        (total_tokens * 100) / max_c
    );
    tracing::info!("{message}");
    emit_status(config, callbacks, "lifecycle", &message);

    // 2026-06-29: pressure snapshot — fired before any compression
    // attempt so the banner can show "即将触发自动压缩" the moment
    // we cross the threshold, not only after the LLM call lands.
    if let Some(ref compressor) = compressor {
        let mut guard: tokio::sync::MutexGuard<'_, ContextCompressor> = compressor.lock().await;
        let pre_snap = guard.breaker_snapshot(
            total_tokens as u64,
            threshold as u64,
            None,
        );
        drop(guard);
        emit_compaction_status(callbacks, &pre_snap);
    }

    // v2: unload the middle of the conversation to disk *before*
    // the LLM compressor runs. Failures are logged, never fatal —
    // the compressor is still our safety net.
    let hermix_home = hermix_config::paths::hermix_home();
    // Forward the session id so the server endpoint
    // `GET /v1/sessions/{id}/archive-refs` can list what we wrote
    // without re-scanning every archive file. Falls back to `None`
    // (no per-session index) when this loop was constructed without
    // a session_id — e.g. the test scaffolding path.
    let session_id = config.session_id.as_deref();
    match archive_context_section(ctx.get_messages(), &hermix_home, None, session_id) {
        Ok(refs) if !refs.is_empty() => {
            tracing::info!(
                "archived {} messages ({} chars) before compression",
                refs.len(),
                refs.iter().map(|r| r.char_count).sum::<usize>(),
            );
            let summary = render_archive_summary(&refs);
            // Surface the refs to the LLM as a guidance block so
            // it knows where to "look up" older context if asked.
            ctx.add_message(Message::system(summary));
            if let Ok(mut slot) = last_archive_refs.lock() {
                *slot = refs;
            }
        }
        Ok(_) => {
            // Empty slice (system + tail only): nothing to unload.
        }
        Err(e) => {
            tracing::warn!("archive_context_section failed: {e}");
        }
    }

    if let Some(note) = memory_pre_compress_note_fn(ctx.get_messages()) {
        ctx.add_message(Message::system(note));
    }

    // Prefer the advanced LLM-powered compressor when available.
    if let Some(ref compressor) = compressor {
        let messages = ctx.get_messages().to_vec();
        let mut guard: tokio::sync::MutexGuard<'_, ContextCompressor> = compressor.lock().await;
        let compressed = guard.compress(messages, None).await;
        // Store the compressed summary for cross-session persistence.
        if let Some(prev) = guard.previous_summary() {
            if let Ok(mut latest) = latest_compressed_summary.lock() {
                *latest = Some(prev.to_string());
            }
        }
        // 2026-06-26 (A + B): stash the raw archive + metrics
        // from this compression so `local_agent_service` can
        // hand them to `save_session_atomic_with_compression` in
        // the same DB transaction that updates
        // `sessions.compressed_summary`. Without this capture,
        // the in-memory conversation would be the only place
        // that knew what got compressed — and `flush_messages`
        // would have wiped those rows from `messages` by the
        // time the agent loop returns to the caller.
        let archived = guard.last_archived_turns().to_vec();
        if !archived.is_empty() {
            if let Ok(mut slot) = latest_compression_archived_turns.lock() {
                *slot = Some(archived);
            }
        }
        if let Ok(mut slot) = latest_compression_metrics.lock() {
            *slot = Some(guard.last_metrics().clone());
        }
        // 2026-06-29: post-attempt snapshot. We classify success
        // by `summary_errors == 0` (compress() records an error
        // in the metrics if the LLM auxiliary call failed) and
        // feed that to `record_compression_outcome` so the
        // breaker state machine actually transitions. Then we
        // snapshot the post-state for the banner.
        //
        // The whole post-processing chain happens *before* the
        // guard is dropped so the snapshot we report reflects
        // the exact state the breaker saw. The callback is
        // synchronous — it doesn't await anything — so the
        // temporary lifetime extension is fine.
        let post_snap = {
            // Snapshot the fields we need *before* taking the
            // mutable borrow on `guard` for
            // `record_compression_outcome` — Rust's NLL won't
            // allow the immutable borrow of `last_metrics()` to
            // overlap with the mutable call otherwise.
            let summary_errors = guard.last_metrics().summary_errors;
            let tokens_saved = guard.last_metrics().tokens_saved;
            let success = summary_errors == 0;
            let post_state = guard.record_compression_outcome(success);
            let used_after = if success && tokens_saved > 0 {
                total_tokens.saturating_sub(tokens_saved as usize)
            } else {
                total_tokens
            };
            let failure_reason = if !success {
                Some("LLM summary generation failed (see server log)".to_string())
            } else {
                None
            };
            let mut snap = guard.breaker_snapshot(
                used_after as u64,
                threshold as u64,
                failure_reason,
            );
            // Pin the post-state so the banner shows the *post-attempt*
            // value even if `breaker_state()` re-derives a HalfOpen
            // because cooldown elapsed during the call.
            snap.state = post_state;
            snap
        };
        emit_compaction_status(callbacks, &post_snap);
        *ctx.get_messages_mut() = compressed;
    } else {
        // Fallback: simple character-based compression.
        ctx.compress();
    }
}