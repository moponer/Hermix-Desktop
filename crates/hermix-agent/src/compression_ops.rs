//! Auto-compression pipeline used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
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
//!
//! ## Background-spawned streaming compression (P1, )
//!
//! To avoid blocking the agent loop on the LLM summary call when
//! context pressure first crosses the trigger threshold (50%), the
//! pipeline now offers two entry points:
//!
//! - [`try_collect_in_flight`] — non-blocking poll of an outstanding
//!   background compression. If the spawn has finished, this rolls
//!   the outcome into the live `ctx` and clears the slot. Otherwise
//!   it returns immediately so the agent loop continues.
//! - [`trigger_spawn_compression`] — fires a `tokio::spawn`-ed
//!   compression. Returns immediately; the agent loop is free to
//!   keep producing turns while the LLM summary call runs in the
//!   background. Result is collected by [`try_collect_in_flight`] on
//!   the next `do_pre_turn_setup` entry.
//! - [`auto_compress_if_over_threshold`] — the **synchronous** entry
//!   point, kept as the high-pressure (>=85%) fallback. When the
//!   context is about to overflow we cannot afford to wait for an
//!   in-flight background spawn; the agent loop calls this directly
//!   to guarantee the pressure drops before the next LLM call.
//!
//! The three entry points share the post-compression bookkeeping
//! (state slots, breaker snapshot, `recompute_total_tokens`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermix_core::{CompressionEngine, CompressionEngineMetrics, Message};
use tokio::sync::Mutex as TokioMutex;

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::archive::{archive_context_section, render_archive_summary, ArchiveRef};
use crate::compression::CompressionMetrics;
use crate::context::ContextManager;
use crate::message_helpers::{emit_compaction_status, emit_status};

/// P1 : hard wall timeout for the high-pressure fallback.
/// When the agent loop's `try_collect_in_flight` is asked to wait for
/// an in-flight background spawn (because pressure is at or above the
/// 85% red-line), we wait at most this long before giving up and
/// cancelling the spawn in favour of the synchronous fast path. Long
/// enough to cover a normal LLM summary round-trip, short enough that
/// the agent loop never visibly stalls on a stuck spawn.
const HIGH_PRESSURE_COLLECT_TIMEOUT: Duration = Duration::from_millis(30_000);

/// P1 : high-pressure red-line. When current pressure is
/// at or above this ratio we cancel any in-flight spawn and run the
/// synchronous fast-compress path immediately — the LLM summary
/// would not have time to finish before the window overflows. The
/// 0.85 figure matches the `>= 0.85 medium` warn ladder in
/// `context.rs` so log lines stay consistent.
const HIGH_PRESSURE_RED_LINE: f64 = 0.85;

/// Outcome of one background-spawned compression attempt — see
/// `agent_loop::CompressionOutcome` (re-exported from `types` via
/// the `pub(crate) use types::*` wildcard in `agent_loop/mod.rs`).
/// Defined here as a private type alias so the spawn/poll/apply
/// triplet can share one name.
pub(crate) type SpawnedOutcome = crate::agent_loop::CompressionOutcome;

/// P1 : background-spawned compression bookkeeping.
/// Re-exported from `agent_loop::types::CompressionInFlight` so call
/// sites in `agent_loop/mod.rs` don't need to reach into the types
/// module directly.
pub(crate) type InFlightCompression = crate::agent_loop::CompressionInFlight;

/// Translate the engine-agnostic `CompressionEngineMetrics` back into
/// the legacy `CompressionMetrics` shape so the downstream
/// `local_agent_service` persistence path doesn't need to change.
/// Same translation as the in-line path in
/// `auto_compress_if_over_threshold` — extracted so the
/// `apply_compression_outcome` call site can reuse it.
pub(crate) fn translate_engine_metrics(
    engine_metrics: &CompressionEngineMetrics,
    trigger_reason: String,
) -> CompressionMetrics {
    CompressionMetrics {
        original_tokens: 0,
        compressed_tokens: 0,
        tokens_saved: engine_metrics.tokens_saved,
        compression_ratio: 0.0,
        original_turns: 0,
        compressed_turns: 0,
        turns_removed: 0,
        tool_results_pruned: 0,
        deduplication_count: 0,
        user_dedup_count: engine_metrics.user_dedup_count as usize,
        pin_count: engine_metrics.pin_count as usize,
        dropped_count: engine_metrics.dropped_count as usize,
        trigger_reason,
        summary_api_calls: 0,
        summary_errors: engine_metrics.summary_errors as u32,
        anti_thrashing_triggered: false,
    }
}

/// Snapshot the live `ctx` state needed to reconstruct an outcome
/// later, and arm a `tokio::spawn` that runs the full compression
/// pipeline in the background. Returns the `JoinHandle` plus the
/// snapshot metadata the agent loop stashes in
/// `AgentLoop::compression_in_flight`.
/// **The agent loop does not block on this call.** The spawn task
/// runs the full `compress()` (LLM summary included); the agent loop
/// continues processing turns and rolls the result in via
/// [`try_collect_in_flight`] on the next `do_pre_turn_setup` entry.
/// **Why the spawn task doesn't touch `self.latest_*` slots**: those
/// slots are plain `std::sync::Mutex`, not `Arc<Mutex>`, so the
/// spawn task (which needs `'static` data) cannot borrow them. The
/// spawn task computes a fully self-contained
/// [`crate::agent_loop::CompressionOutcome`] (with the post-snap
/// already filled in), and the apply path on the main loop body
/// updates the slots. This keeps the spawn task a pure-function-of-
/// snapshot without mutating the agent loop's bookkeeping.
/// The caller is responsible for injecting the memory pre-compress
/// note into `snapshot_messages` *before* calling this function —
/// we can't borrow `self` from inside the spawn task and we want to
/// keep the snapshot immutable from the task's perspective.
/// Pre-conditions:
/// - `compressor` is `Some` (otherwise we have nothing to spawn — the
/// caller should have already tried the no-op fallback).
/// - `in_flight.is_none()` — call sites must check before invoking.
/// The caller (`maybe_spawn_compression` on `AgentLoop`) enforces
/// this invariant.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trigger_spawn_compression(
    config: &AgentConfig,
    compressor: Arc<TokioMutex<dyn CompressionEngine + Send>>,
    snapshot_messages: Vec<Message>,
    _snapshot_len: usize,
    _snapshot_tokens: usize,
    _max_context_tokens: usize,
    _pressure_at_trigger: f64,
    _threshold_tokens: u64,
) -> tokio::task::JoinHandle<SpawnedOutcome> {
    let _started_at = Instant::now();
    // Archive summary is inserted into the snapshot *before* the
    // compressor runs, mirroring what the synchronous path does
    // inline. We move the snapshot into the task so the agent loop
    // body doesn't need to hold a `&mut` borrow across the await.
    let mut working = snapshot_messages;

    // Archive the middle of the conversation to disk *before* the
    // compressor runs. The archive is a sync best-effort — we do it
    // inside the spawn so the agent loop body doesn't pay for it.
    // The refs are stashed in the spawn outcome (not in the
    // agent-loop's `last_archive_refs` slot) so the apply path on
    // the main loop body can write them there without crossing an
    // `'static` boundary.
    let hermix_home = hermix_config::paths::hermix_home();
    let session_id = config.session_id.clone();
    let mut archive_refs: Vec<ArchiveRef> = Vec::new();
    match archive_context_section(&working, &hermix_home, None, session_id.as_deref()) {
        Ok(refs) if !refs.is_empty() => {
            let summary_text = render_archive_summary(&refs);
            working.push(Message::system(summary_text));
            archive_refs = refs;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "spawn compression: archive_context_section failed");
        }
    }

    tokio::spawn(async move {
        // Lock the compressor ONLY for the (fast, non-LLM) compression
        // pass. We deliberately use `compress_fast` here — NOT `compress`
        // — so we never hold the shared engine lock across an LLM
        // round-trip. That keeps the per-turn `auto_compress_if_over_threshold`
        // path (which does take the lock for the real LLM summarize) from
        // blocking behind this background task. The background pass is a
        // best-effort coarse compress; the per-turn path does the heavy
        // LLM summarization when pressure actually crosses threshold.
        let mut guard = compressor.lock().await;
        let compressed = guard.compress_fast(working).await;
        // The engine-side state slots (previous_summary,
        // last_archived_turns, last_metrics) get updated by
        // `compress` — the apply path reads them back via the
        // engine on the main loop body. We only need to flag
        // success so the apply path can decide whether to log a
        // warning vs info line.
        let engine_metrics = guard.last_metrics();
        let summary_errors = engine_metrics.summary_errors;
        let success = summary_errors == 0;

        // Drive the breaker so the next pre-attempt snapshot sees
        // the post-attempt state. The breaker state lives on the
        // engine, so we drop the guard before returning.
        let post_state = guard.record_outcome(success);
        let _ = post_state; // surfaced via breaker_snapshot reads on the apply path.
        drop(guard);

        SpawnedOutcome {
            compressed_base: compressed,
            archive_refs,
            success,
        }
    })
}

/// Roll a finished background-spawn outcome into the live `ctx`.
/// Merge semantics: the spawn was triggered with a snapshot of the
/// first `snapshot_len` messages. While the spawn was running, the
/// agent loop may have appended more messages (new user turns, new
/// assistant responses, new tool results). We preserve those by
/// splicing them in at the end of the compressed base:
/// ```text
/// ctx.messages = compressed_base (from spawn) + new_messages_since_snapshot
/// ```
/// `compressed_base` is whatever the compressor returned from
/// `compress(snapshot)` — typically `[kept_head, summary_msg]` —
/// so the merge is straightforward: drop everything from
/// `snapshot_len` onwards in the live `ctx`, replace with the
/// compressed base, then re-append the new messages.
pub(crate) fn apply_compression_outcome(
    outcome: SpawnedOutcome,
    snapshot_len: usize,
    snapshot_tokens: usize,
    ctx: &mut ContextManager,
    last_archive_refs: &Mutex<Vec<ArchiveRef>>,
) {
    // SAFETY: the length-based merge below assumes the live context's
    // prefix of `snapshot_len` messages is unchanged since the spawn
    // snapshot (i.e. only *appends* happened during the background
    // compress). The context budget-gate can `drain`/insert mid-list,
    // which would make `messages[snapshot_len..]` include messages that
    // were already part of the snapshot → silent duplication/loss.
    // `Message` has no stable id to splice by today, so if the live
    // prefix shrank below the snapshot point we skip the merge entirely
    // (no corruption); the next per-turn compression re-derives a
    // correct state. A full id-based splice is the proper follow-up.
    let live_len = ctx.get_messages().len();
    if live_len < snapshot_len {
        tracing::warn!(
            snapshot_len,
            live_len,
            "skipping background compression merge: live context shorter than snapshot (prefix drained)"
        );
        return;
    }
    let new_messages: Vec<Message> = ctx.get_messages()[snapshot_len..].to_vec();
    let merged_len = outcome.compressed_base.len() + new_messages.len();
    tracing::info!(
        snapshot_len,
        compressed_base_len = outcome.compressed_base.len(),
        new_messages_len = new_messages.len(),
        merged_len,
        success = outcome.success,
        "applying background-spawned compression outcome"
    );
    // Forward the archive refs the spawn task produced. The slot
    // is a plain `std::sync::Mutex` (not `Arc<Mutex>`); since we're
    // already on the agent loop's main thread, a brief lock here is
    // safe and contention-free in practice.
    if !outcome.archive_refs.is_empty() {
        if let Ok(mut slot) = last_archive_refs.lock() {
            *slot = outcome.archive_refs.clone();
        }
    }
    let mut merged = outcome.compressed_base;
    merged.extend(new_messages);
    *ctx.get_messages_mut() = merged;
    ctx.recompute_total_tokens();
    // Best-effort log line — we can't easily log the live
    // `total_tokens` delta without re-reading the cache, but we know
    // the *starting* token count from the snapshot.
    let _ = snapshot_tokens;
}

/// P1 : poll the agent loop's in-flight background
/// compression. If the spawn has finished, roll the outcome into
/// `ctx` and clear the slot. Otherwise return without touching the
/// ctx.
/// Returns `true` when an outcome was applied, `false` when no
/// spawn was in flight or the spawn is still running.
/// The `wait_for_finish` argument controls whether we await the
/// `JoinHandle` with a timeout. The high-pressure red-line path
/// (>=85%) sets `wait_for_finish = true` so we can roll an
/// almost-done spawn in without falling back to the synchronous fast
/// path. The normal pre-turn path sets `wait_for_finish = false` so
/// the agent loop never blocks on the spawn.
/// **Concurrency note**: we `Option::take()` the slot so the spawn
/// task's `JoinHandle` is owned by this function for the duration of
/// the await. Other concurrent polls (none in practice — the agent
/// loop is single-threaded) would see `None` and skip; the spawn
/// task itself is unaffected because the handle is now locally
/// owned.
pub(crate) async fn try_collect_in_flight(
    in_flight: &Mutex<Option<InFlightCompression>>,
    ctx: &mut ContextManager,
    last_archive_refs: &Mutex<Vec<ArchiveRef>>,
    wait_for_finish: bool,
) -> bool {
    // 1. Take the slot so we own the JoinHandle. The slot is set
    // back to `None` while we await, so the state machine is
    // consistent regardless of which branch we exit from.
    let in_flight_record = match in_flight.lock() {
        Ok(mut slot) => match slot.take() {
            Some(ifr) => ifr,
            None => return false,
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "try_collect_in_flight: in_flight mutex poisoned, dropping"
            );
            return false;
        }
    };
    let InFlightCompression {
        handle,
        snapshot_len,
        snapshot_tokens,
        started_at,
        max_context_tokens,
    } = in_flight_record;

    // 2. Decide whether to await. `JoinHandle::is_finished` is the
    // cheap non-blocking probe; when the caller wants to block we
    // `tokio::time::timeout` so the agent loop never stalls
    // forever on a stuck spawn.
    let joined = if wait_for_finish {
        match tokio::time::timeout(HIGH_PRESSURE_COLLECT_TIMEOUT, handle).await {
            Ok(joined) => joined,
            Err(_) => {
                tracing::warn!(
                    timeout_ms = HIGH_PRESSURE_COLLECT_TIMEOUT.as_millis() as u64,
                    "high-pressure collect timed out, abandoning spawn"
                );
                // The spawn task is detached at this point (its
                // JoinHandle was dropped by `timeout`); the agent
                // loop continues without rolling anything in.
                return false;
            }
        }
    } else if handle.is_finished() {
        handle.await
    } else {
        // Not finished yet and we're not allowed to block — put
        // the slot back so the next poll can re-check.
        if let Ok(mut slot) = in_flight.lock() {
            // The slot should still be `None` because we `take`-ed
            // it above. If a concurrent caller raced (none in
            // practice) we lose the spawn — log and move on.
            if slot.is_none() {
                *slot = Some(InFlightCompression {
                    handle,
                    snapshot_len,
                    snapshot_tokens,
                    started_at,
                    max_context_tokens,
                });
            } else {
                tracing::warn!(
                    "try_collect_in_flight: slot unexpectedly populated, \
                     abandoning in-flight spawn"
                );
            }
        }
        return false;
    };

    // 3. Apply if the spawn produced a usable outcome. A panic in
    // the spawn task is logged and the slot stays cleared, but
    // the agent loop continues with the un-compressed `ctx`.
    let outcome = match joined {
        Ok(o) => o,
        Err(join_err) => {
            tracing::warn!(
                error = %join_err,
                "background-spawned compression task panicked, clearing slot"
            );
            return false;
        }
    };

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    tracing::info!(
        elapsed_ms,
        snapshot_len,
        snapshot_tokens,
        max_context_tokens,
        success = outcome.success,
        archive_refs_count = outcome.archive_refs.len(),
        "background-spawned compression finished, applying"
    );
    apply_compression_outcome(outcome, snapshot_len, snapshot_tokens, ctx, last_archive_refs);
    true
}

/// Auto-compress when context pressure crosses the configured
/// threshold. **Synchronous** — blocks the calling turn on the LLM
/// summary call. Used in two paths:
/// 1. The `preflight_context_compress` flag, where the caller is
/// happy to wait because no LLM call has happened yet.
/// 2. The high-pressure red-line fallback in
/// `AgentLoop::do_pre_turn_setup`: when pressure is at or above
/// [`HIGH_PRESSURE_RED_LINE`], the agent loop cancels any
/// in-flight background spawn and runs this synchronously to
/// guarantee the pressure drops before the next LLM call.
/// The pipeline:
/// 1. Compute pressure and emit a `lifecycle` status callback.
/// 2. Pre-attempt breaker snapshot for the `CompactionStatusBanner`.
/// 3. Archive the middle of the conversation to disk (best-effort).
/// 4. Inject the archive summary into the context so the LLM knows
/// where to "look up" older context if asked.
/// 5. Inject the `MemoryRouter::on_pre_compress` note (best-effort).
/// 6. Run the LLM compressor (if wired) and update the live
/// compression-state slots; otherwise fall back to the simple
/// character-based `ContextManager::compress`.
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
#[tracing::instrument(
    name = "auto_compress_if_over_threshold",
    level = "info",
    skip_all,
    fields(
        total_tokens = tracing::field::Empty,
        threshold_tokens = tracing::field::Empty,
        pressure_pct = tracing::field::Empty,
        outcome = tracing::field::Empty,
        archive_refs = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    )
)]
pub async fn auto_compress_if_over_threshold<F>(
    config: &AgentConfig,
    callbacks: &AgentCallbacks,
    compressor: Option<&Arc<TokioMutex<dyn CompressionEngine + Send>>>,
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

    // pressure snapshot — fired before any compression
    // attempt so the banner can show "即将触发自动压缩" the moment
    // we cross the threshold, not only after the LLM call lands.
    // stamped `phase = "starting"` so the
    // `CompactionStatusBanner` shows the user that work is about
    // to begin — without this the banner jumped straight from
    // the pressure warning to the post-attempt result, with no
    // visible "压缩中" transition in between.
    if let Some(ref compressor) = compressor {
        let mut guard: tokio::sync::MutexGuard<'_, dyn CompressionEngine + Send> =
            compressor.lock().await;
        // Respect `should_compress` as an early opt-out: engines like
        // `NoOpCompressionEngine` advertise themselves as "never
        // compress" precisely so misconfigurations (user disabled
        // compression, LLM unavailable, ...) don't quietly write
        // archive refs that no summary will ever cover. The pre-snap
        // is only emitted when the engine has confirmed it will run.
        if !guard.should_compress(total_tokens as u64) {
            return;
        }
        let pre_snap = guard.breaker_snapshot_with_phase(
            total_tokens as u64,
            threshold as u64,
            max_c as u64,
            None,
            "starting",
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
        // (P1): pressure-tiered compression. Moderate
        // pressure (< 85%) takes the fast path — archive + dedup
        // + score-drop + static fallback, no LLM call, so the
        // agent loop doesn't block a turn on an LLM round-trip.
        // High pressure (>= 85%) takes the full LLM summary path
        // so the model gets a real handoff before the window
        // overflows. The 0.85 tier matches the `>= 0.85 medium`
        // warn ladder in `context.rs` so log lines stay consistent.
        let pressure = total_tokens as f64 / max_c as f64;
        let use_fast_path = pressure < HIGH_PRESSURE_RED_LINE;
        let in_progress_phase = if use_fast_path {
            "in_progress_fast"
        } else {
            "in_progress_llm"
        };
        // emit an `in_progress` snapshot just before the
        // actual work begins so the front-end `CompactionStatusBanner`
        // can render its spinner / "正在压缩中" pill during the LLM
        // round-trip (5–30s on real models). The post-attempt snapshot
        // below will then transition to `"complete"` / `"failed"` to
        // close the loop.
        {
            let mut guard: tokio::sync::MutexGuard<'_, dyn CompressionEngine + Send> =
                compressor.lock().await;
            let in_progress_snap = guard.breaker_snapshot_with_phase(
                total_tokens as u64,
                threshold as u64,
                max_c as u64,
                None,
                in_progress_phase,
            );
            drop(guard);
            emit_compaction_status(callbacks, &in_progress_snap);
        }
        // Now do the actual compression work — pressure-tiered.
        let mut guard: tokio::sync::MutexGuard<'_, dyn CompressionEngine + Send> =
            compressor.lock().await;
        let compressed = if use_fast_path {
            tracing::info!(
                "moderate pressure ({:.0}%) — using fast compression (no LLM summary)",
                pressure * 100.0
            );
            guard.compress_fast(messages).await
        } else {
            tracing::info!(
                "high pressure ({:.0}%) — using full LLM compression",
                pressure * 100.0
            );
            guard.compress(messages).await
        };
        // Store the compressed summary for cross-session persistence.
        if let Some(prev) = guard.previous_summary() {
            if let Ok(mut latest) = latest_compressed_summary.lock() {
                *latest = Some(prev.to_string());
            }
        }
        // (A + B): stash the raw archive + metrics
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
            let engine_metrics = guard.last_metrics();
            // (): translate the engine-agnostic
            // metric surface back into the
            // built-in `CompressionMetrics` shape so the
            // downstream `local_agent_service` persistence path
            // doesn't need to change. We map the engine's
            // `tokens_saved` / `summary_errors` /
            // `compression_count` onto the corresponding fields
            // and leave the rest at zero (they were only ever
            // populated by the LLM compressor anyway, so
            // plugins that don't track them get a zero row).
            *slot = Some(translate_engine_metrics(
                &engine_metrics,
                engine_metrics.trigger_reason.clone(),
            ));
        }
        // post-attempt snapshot. We classify success
        // by `summary_errors == 0` (compress() records an error
        // in the metrics if the LLM auxiliary call failed) and
        // feed that to `record_compression_outcome` so the
        // breaker state machine actually transitions. Then we
        // snapshot the post-state for the banner.
        // also stamp `phase = "complete"` / `"failed"`
        // so the `CompactionStatusBanner` can close the "正在压缩中"
        // spinner that the `in_progress` snapshot opened.
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
            let post_state = guard.record_outcome(success);
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
            let phase = if success { "complete" } else { "failed" };
            let mut snap = guard.breaker_snapshot_with_phase(
                used_after as u64,
                threshold as u64,
                max_c as u64,
                failure_reason,
                phase,
            );
            // Pin the post-state so the banner shows the *post-attempt*
            // value even if `breaker_state()` re-derives a HalfOpen
            // because cooldown elapsed during the call.
            snap.state = post_state;
            snap
        };
        emit_compaction_status(callbacks, &post_snap);
        *ctx.get_messages_mut() = compressed;
        // Bulk-replace invalidates the cached running token total; the
        // agent loop's next pressure check calls `total_tokens()` and
        // would otherwise read the pre-compression count.
        ctx.recompute_total_tokens();
    } else {
        // Fallback: simple character-based compression. The method is
        // deprecated as a no-op fallback only — when no LLM compressor
        // is wired, we still call it for symmetry with the historical
        // contract; it intentionally does nothing, so the agent loop
        // continues with the un-truncated messages and the next
        // pressure check will re-evaluate.
        #[allow(deprecated)]
        ctx.compress();
    }
}

/// Convenience: read the live in-flight slot to decide whether the
/// agent loop is allowed to fire another spawn. Returns `true` when
/// the slot is empty (i.e. safe to spawn) and `false` when a previous
/// spawn is still running (caller should fall through to the high-
/// pressure red-line synchronous path or simply skip).
pub(crate) fn in_flight_slot_is_empty(
    in_flight: &Mutex<Option<InFlightCompression>>,
) -> bool {
    match in_flight.lock() {
        Ok(slot) => slot.is_none(),
        Err(_) => true, // poisoned — pretend it's empty so we try to recover
    }
}

/// Install a finished in-flight handle into the slot. Used by
/// `AgentLoop::maybe_spawn_compression` after building the spawn
/// plan.
pub(crate) fn install_in_flight(
    in_flight: &Mutex<Option<InFlightCompression>>,
    new_in_flight: InFlightCompression,
) {
    match in_flight.lock() {
        Ok(mut slot) => {
            *slot = Some(new_in_flight);
        }
        Err(e) => {
            tracing::warn!(error = %e, "install_in_flight: slot mutex poisoned");
        }
    }
}

/// Public re-export of the red-line ratio for callers that want to
/// log a tier label without hard-coding 0.85.
pub(crate) const HIGH_PRESSURE_RED_LINE_RATIO: f64 = HIGH_PRESSURE_RED_LINE;

// (): end-to-end tests for the
// compression-pathway pipeline. These exercise the full
// auto_compress → archive → LLM/NoOp → snapshot
// flow without a real LLM, by using NoOpCompressionEngine to
// confirm the trait-dispatch wiring works () and the
// Integration tests for the compression pipeline entry point
// live in `tests/compression_ops_integration.rs` ().

#[cfg(test)]
mod spawn_streaming_tests {
    //! P1 : unit tests for the background-spawned
    //! streaming compression pipeline. These live alongside the
    //! implementation (rather than in
    //! `tests/compression_ops_integration.rs`) because they need
    //! access to the `pub(crate)` trigger/collect/apply triplet —
    //! the public AgentLoop surface is too coarse to exercise the
    //! race / merge semantics directly.
    //!
    //! Engine used: a custom [`SleepingCompressionEngine`] that
    //! blocks for a configurable duration inside `compress()`. This
    //! lets the tests drive the spawn/poll race deterministically
    //! without spinning up a real LLM.

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use hermix_core::{
        CompressionEngine, CompressionEngineMetrics, CompressionEngineSnapshot,
        CompressionEngineState, CoreContextEngine, Message,
    };
    use tokio::sync::Mutex as TokioMutex;

    use super::*;

    /// Test engine that sleeps for `delay_ms` inside `compress()`,
    /// then returns the messages unchanged. Tracks the number of
    /// times `compress()` was called so tests can assert
    /// "exactly one spawn fired".
    struct SleepingCompressionEngine {
        delay_ms: u64,
        call_count: AtomicU64,
        // `OnceLock` gives us a stable `&str` reference for
        // `previous_summary` without dragging the `Mutex` borrow
        // across the trait method's return type.
        last_summary: std::sync::OnceLock<String>,
    }

    impl SleepingCompressionEngine {
        fn new(delay_ms: u64) -> Self {
            Self {
                delay_ms,
                call_count: AtomicU64::new(0),
                last_summary: std::sync::OnceLock::new(),
            }
        }
    }

    #[async_trait]
    impl CoreContextEngine for SleepingCompressionEngine {
        fn should_compress(&self, _current_tokens: u64) -> bool {
            true
        }

        async fn compress(&mut self, messages: Vec<Message>) -> Vec<Message> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            // Pass-through, but stamp a summary so we can verify
            // the post-apply slot refresh reads it.
            let _ = self
                .last_summary
                .set(format!("summary-of-{}", messages.len()));
            messages
        }
    }

    #[async_trait]
    impl CompressionEngine for SleepingCompressionEngine {
        fn previous_summary(&self) -> Option<&str> {
            self.last_summary.get().map(|s| s.as_str())
        }
        fn last_archived_turns(&self) -> &[Message] {
            &[]
        }
        fn last_metrics(&self) -> CompressionEngineMetrics {
            CompressionEngineMetrics::default()
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
                // the test engine doesn't drive any
                // compression-phase semantics; defaults to `"idle"`.
                phase: "idle".to_string(),
            }
        }
        fn breaker_snapshot_with_phase(
            &mut self,
            current_tokens: u64,
            threshold_tokens: u64,
            max_tokens: u64,
            failure_reason: Option<String>,
            phase: &str,
        ) -> CompressionEngineSnapshot {
            CompressionEngineSnapshot {
                state: CompressionEngineState::Closed,
                current_tokens,
                threshold_tokens,
                max_tokens,
                failure_reason,
                phase: phase.to_string(),
            }
        }
        fn record_outcome(&mut self, _success: bool) -> CompressionEngineState {
            CompressionEngineState::Closed
        }
    }

    fn make_ctx_with_n_messages(n: usize, max_context: usize) -> ContextManager {
        let mut ctx = ContextManager::new(max_context).with_output_reserve(0);
        for i in 0..n {
            ctx.add_message(Message::user(format!("msg-{i}")));
        }
        ctx
    }

    /// T1: `trigger_spawn_compression` returns immediately without
    /// waiting for the LLM call. We use a 500ms-sleep engine and
    /// assert the spawn function returns in well under that bound.
    #[tokio::test]
    async fn trigger_spawn_compression_returns_without_blocking_on_llm() {
        let engine = Arc::new(TokioMutex::new(SleepingCompressionEngine::new(500)));
        let snapshot = vec![Message::user("hello")];
        let started = std::time::Instant::now();
        let handle = trigger_spawn_compression(
            &AgentConfig::default(),
            engine.clone(),
            snapshot,
            1,
            100,
            1000,
            0.6,
            500,
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "trigger_spawn_compression should not block on the LLM call; \
             took {:?}",
            elapsed
        );
        // Let the spawn finish so the test exits cleanly.
        let _ = handle.await;
    }

    /// T2: `try_collect_in_flight` returns `false` when the slot
    /// is empty.
    #[tokio::test]
    async fn try_collect_returns_false_when_slot_empty() {
        let slot: std::sync::Mutex<Option<InFlightCompression>> =
            std::sync::Mutex::new(None);
        let mut ctx = make_ctx_with_n_messages(3, 10_000);
        let archive_refs: std::sync::Mutex<Vec<ArchiveRef>> = std::sync::Mutex::new(Vec::new());
        let applied = try_collect_in_flight(&slot, &mut ctx, &archive_refs, false).await;
        assert!(!applied);
    }

    /// T3: end-to-end — spawn fires, slot populated, while the
    /// spawn is still running `try_collect` returns `false`; once
    /// the spawn completes, the next `try_collect` rolls the
    /// outcome in.
    #[tokio::test]
    async fn poll_in_flight_returns_false_until_spawn_completes() {
        let engine = Arc::new(TokioMutex::new(SleepingCompressionEngine::new(300)));
        let snapshot = vec![
            Message::user("a"),
            Message::assistant("b"),
            Message::user("c"),
        ];
        let handle = trigger_spawn_compression(
            &AgentConfig::default(),
            engine.clone(),
            snapshot.clone(),
            snapshot.len(),
            300,
            1000,
            0.6,
            500,
        );
        let in_flight = std::sync::Mutex::new(Some(InFlightCompression {
            handle,
            snapshot_len: snapshot.len(),
            snapshot_tokens: 300,
            started_at: std::time::Instant::now(),
            max_context_tokens: 1000,
        }));
        let mut ctx = make_ctx_with_n_messages(snapshot.len(), 10_000);
        let archive_refs: std::sync::Mutex<Vec<ArchiveRef>> = std::sync::Mutex::new(Vec::new());

        // 1st poll — should return false because the spawn is
        // still sleeping.
        let applied = try_collect_in_flight(&in_flight, &mut ctx, &archive_refs, false).await;
        assert!(!applied, "spawn still sleeping, poll should skip");

        // 2nd poll — wait until the spawn is done, then collect.
        // We block here (wait_for_finish = true) for a generous
        // timeout.
        let applied = try_collect_in_flight(&in_flight, &mut ctx, &archive_refs, true).await;
        assert!(applied, "spawn should have completed by now");
        // Pass-through engine — the spawn's compressed base is
        // `snapshot` plus the archive-summary system message the
        // spawn inserts (3 + 1 = 4). The pre-spawn ctx had 3
        // messages; the apply path replaces them with the
        // compressed base, so the final length is 4.
        let expected_len = snapshot.len() + 1;
        assert_eq!(
            ctx.get_messages().len(),
            expected_len,
            "ctx should hold compressed_base (snapshot + archive summary), \
             got {} messages, expected {}",
            ctx.get_messages().len(),
            expected_len,
        );
        assert_eq!(
            ctx.get_messages()[0].content.as_deref(),
            Some("a")
        );
    }

    /// T4: `try_collect_in_flight` with `wait_for_finish = false`
    /// and a finished spawn returns the outcome (the non-blocking
    /// path also handles the finished-and-ready case).
    #[tokio::test]
    async fn poll_in_flight_applies_immediately_when_already_finished() {
        let engine = Arc::new(TokioMutex::new(SleepingCompressionEngine::new(0)));
        // Spawn and wait for it to fully complete *outside* the
        // slot, then put a finished JoinHandle into the slot.
        // `JoinHandle::is_finished()` is reliable once the
        // background task has actually polled to completion; we
        // ensure that by `await`-ing the handle on a separate
        // JoinHandle here and just placing `handle2` after a
        // bounded sleep so the runtime has scheduled the spawn.
        let snapshot = vec![Message::user("only-msg")];
        let handle2 = trigger_spawn_compression(
            &AgentConfig::default(),
            engine.clone(),
            snapshot.clone(),
            snapshot.len(),
            100,
            1000,
            0.6,
            500,
        );
        // Give the spawn enough wall-clock to finish (delay=0 +
        // archive + record_outcome). 200ms is generous on any
        // test runner.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            handle2.is_finished(),
            "spawn should have completed within 200ms"
        );
        let in_flight = std::sync::Mutex::new(Some(InFlightCompression {
            handle: handle2,
            snapshot_len: snapshot.len(),
            snapshot_tokens: 100,
            started_at: std::time::Instant::now(),
            max_context_tokens: 1000,
        }));
        let mut ctx = make_ctx_with_n_messages(snapshot.len(), 10_000);
        let archive_refs: std::sync::Mutex<Vec<ArchiveRef>> = std::sync::Mutex::new(Vec::new());
        // Non-blocking poll. If the spawn already finished, we
        // should get `applied = true` without any wait.
        let applied = try_collect_in_flight(&in_flight, &mut ctx, &archive_refs, false).await;
        assert!(applied, "non-blocking poll should collect a finished spawn");
    }

    /// T5: `apply_compression_outcome` correctly splices the
    /// compressed base + new messages added after the snapshot.
    /// This is the critical merge correctness invariant.
    #[tokio::test]
    async fn apply_outcome_preserves_new_messages_after_snapshot() {
        let _snapshot = vec![
            Message::user("a"),
            Message::assistant("b"),
            Message::user("c"),
        ];
        // Simulate the agent loop appending a new message after
        // the snapshot was taken but before the spawn finished.
        let mut ctx = make_ctx_with_n_messages(3, 10_000);
        ctx.add_message(Message::user("d"));
        ctx.add_message(Message::user("e"));
        // The compressor returned a "compressed" view of just the
        // snapshot — say it dropped message "c" and replaced it
        // with a summary. We don't actually run the compressor;
        // we build the outcome by hand.
        let outcome = SpawnedOutcome {
            compressed_base: vec![Message::user("a"), Message::assistant("b")],
            archive_refs: vec![],
            success: true,
        };
        let last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>> =
            std::sync::Mutex::new(Vec::new());
        apply_compression_outcome(outcome, 3, 300, &mut ctx, &last_archive_refs);
        // Final ctx: [a, b] (compressed base) + [d, e] (post-snapshot).
        assert_eq!(ctx.get_messages().len(), 4);
        assert_eq!(ctx.get_messages()[0].content.as_deref(), Some("a"));
        assert_eq!(ctx.get_messages()[1].content.as_deref(), Some("b"));
        assert_eq!(ctx.get_messages()[2].content.as_deref(), Some("d"));
        assert_eq!(ctx.get_messages()[3].content.as_deref(), Some("e"));
    }

    /// T6: the slot helpers (`in_flight_slot_is_empty`,
    /// `install_in_flight`) compose correctly: empty → install →
    /// non-empty → after `take()` → empty again.
    #[tokio::test]
    async fn in_flight_slot_lifecycle() {
        let engine = Arc::new(TokioMutex::new(SleepingCompressionEngine::new(0)));
        let slot: std::sync::Mutex<Option<InFlightCompression>> =
            std::sync::Mutex::new(None);
        assert!(in_flight_slot_is_empty(&slot));
        let handle = trigger_spawn_compression(
            &AgentConfig::default(),
            engine.clone(),
            vec![Message::user("x")],
            1,
            100,
            1000,
            0.6,
            500,
        );
        install_in_flight(
            &slot,
            InFlightCompression {
                handle,
                snapshot_len: 1,
                snapshot_tokens: 100,
                started_at: std::time::Instant::now(),
                max_context_tokens: 1000,
            },
        );
        assert!(!in_flight_slot_is_empty(&slot));
        // Drain the slot via a fake poll with an empty ctx.
        let mut ctx = make_ctx_with_n_messages(1, 10_000);
        let archive_refs: std::sync::Mutex<Vec<ArchiveRef>> = std::sync::Mutex::new(Vec::new());
        let _ = try_collect_in_flight(&slot, &mut ctx, &archive_refs, true).await;
        assert!(in_flight_slot_is_empty(&slot));
    }

    /// T7: spawn task panic is handled gracefully — slot is
    /// cleared and `try_collect` returns false instead of
    /// propagating the panic.
    #[tokio::test]
    async fn spawn_task_panic_clears_slot_without_propagating() {
        // Build a JoinHandle that is already-finished and
        // represents a panic. We do this by spawning a closure
        // that panics and waiting for it.
        let handle: tokio::task::JoinHandle<SpawnedOutcome> = tokio::spawn(async move {
            panic!("deliberate spawn panic for test");
        });
        // Wait for the panic to manifest as a JoinError.
        let join_result = handle.await;
        assert!(join_result.is_err(), "expected panic");
        // Now build an in_flight slot with an already-panicked
        // JoinHandle. We simulate this by constructing a
        // JoinHandle for a task we know has panicked.
        let panicked_handle: tokio::task::JoinHandle<SpawnedOutcome> =
            tokio::spawn(async move {
                panic!("test panic");
            });
        // Give it a moment to actually panic.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let slot: std::sync::Mutex<Option<InFlightCompression>> = std::sync::Mutex::new(Some(
            InFlightCompression {
                handle: panicked_handle,
                snapshot_len: 0,
                snapshot_tokens: 0,
                started_at: std::time::Instant::now(),
                max_context_tokens: 1000,
            },
        ));
        let mut ctx = make_ctx_with_n_messages(1, 10_000);
        let archive_refs: std::sync::Mutex<Vec<ArchiveRef>> = std::sync::Mutex::new(Vec::new());
        // wait_for_finish = true so the timeout-await path runs
        // and we exercise the panic-handling branch.
        let applied = try_collect_in_flight(&slot, &mut ctx, &archive_refs, true).await;
        assert!(!applied, "panic should not propagate to caller");
        assert!(in_flight_slot_is_empty(&slot), "slot should be cleared");
    }
}
