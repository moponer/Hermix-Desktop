//! Focus/memory bridging and tool-error accounting.

use serde_json::Value;

use super::*;

impl AgentLoop {
    /// Drain pending `FocusConclusion`s from the focus stack and
    /// forward them to the long-term memory backend. Returns the
    /// number of conclusions processed.
    /// No-op if `memory_router` is `None` (conclusions are still
    /// drained from the focus stack — they don't pile up).
    /// this now actually persists via the router's
    /// `upsert_conclusion` fan-out. Backends that don't implement
    /// the write surface (8 HTTPS plugins) return
    /// `MemoryError::NotImplemented` and the router skips them
    /// silently. Only the embedded SQLite backend (and any future
    /// write-capable backend) actually writes to long-term store.
    pub fn drain_focus_conclusions_to_memory_router(&self) -> usize {
        let session_id = self.config.session_id.clone().unwrap_or_default();
        let count = crate::memory_prefetch::drain_focus_conclusions(
            &self.focus_stack,
            self.memory_router.as_ref(),
            &session_id,
        );
        // surface drained focus conclusions as "总结" rows.
        if count > 0 && !self.config.quiet_mode {
            self.emit_memory_update(&crate::memory_prefetch::MemoryWriteRecord {
                action: crate::memory_prefetch::MemoryAction::Summarize,
                memory_type: crate::memory_prefetch::MemoryKind::Summary,
                key: "session".to_string(),
                summary: format!("归档了 {count} 条会话结论"),
                count: Some(count as u64),
            });
        }
        count
    }

    /// Pre-LLM-call drain: inject steer into the last tool result if possible; otherwise restash.
    pub(crate) fn pre_api_apply_pending_steer(&self, ctx: &mut ContextManager) {
        crate::steer::pre_api_apply_pending_steer(&self.pending_steer, ctx)
    }

    /// Post-tool-call apply: inject steer into the last tool result if pending.
    pub(crate) fn post_tool_apply_pending_steer(&self, ctx: &mut ContextManager) {
        crate::steer::post_tool_apply_pending_steer(&self.pending_steer, ctx)
    }

    // -- Plugin hook helpers ------------------------------------------------

    pub(crate) fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        crate::hook_dispatch::invoke_hook(self.plugin_manager.as_ref(), hook, ctx_val)
    }

    pub(crate) fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        crate::hook_dispatch::inject_hook_context(results, ctx)
    }

    // -- Memory helpers ----------------------------------------------------

    pub(crate) async fn memory_prefetch(&self, query: &str, session_id: &str, messages: Option<&[Message]>) -> String {
        crate::memory_prefetch::memory_prefetch(
            &self.config,
            self.memory_router.as_deref(),
            self.embedding_provider.as_deref(),
            &self.memory_recall_seeded,
            self.config.hermix_home.as_deref().map(std::path::Path::new),
            query,
            session_id,
            messages,
        )
        .await
    }

    pub(crate) fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        let records = crate::memory_prefetch::memory_sync(
            &self.config,
            self.memory_router.as_deref(),
            user,
            assistant,
            session_id,
        );
        // surface per-turn fact/style writes as a single
        // batched "记住了 N 条事实/偏好" chat row so the user can see
        // what the agent is remembering without flooding the stream.
        if !records.is_empty() && !self.config.quiet_mode {
            let count = records.len() as u64;
            let listed: String = records
                .iter()
                .take(3)
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>()
                .join("、");
            let more = if records.len() > 3 {
                format!(" 等 {count} 条")
            } else {
                String::new()
            };
            self.emit_memory_update(&crate::memory_prefetch::MemoryWriteRecord {
                action: crate::memory_prefetch::MemoryAction::Write,
                memory_type: crate::memory_prefetch::MemoryKind::Fact,
                key: "memory".to_string(),
                summary: format!("记住 {count} 条事实/偏好：{listed}{more}"),
                count: Some(count),
            });
        }
    }

    /// (架构-1): freeze the current session state into a
    /// `session_snapshots` row. Called every N turns (default 50)
    /// and on `on_session_end`. The `frame_ids` and `summary` are
    /// extracted from the router's embedded backend so this method
    /// is a no-op when the embedded backend is absent.
    /// Designed to be cheap: the embedded SQLite path is O(visible
    /// frames) on `total_turns.is_multiple_of(N)` boundary calls,
    /// and O(1) on all other turns.
    pub(crate) fn maybe_freeze_snapshot(&self, session_id: &str, turn_number: u32) {
        crate::memory_prefetch::maybe_freeze_snapshot(
            &self.config,
            self.memory_router.as_deref(),
            session_id,
            turn_number,
        )
    }

    /// (架构-2): enforce a hard cap on visible frames so
    /// memory.db doesn't grow unbounded. Called every 100 turns as a
    /// background cleanup. Default cap is 10000 — enough for ~3
    /// months of heavy chat, soft-hidden rows stay on disk for the
    /// audit trail and the 30-day janitor.
    pub(crate) fn maybe_enforce_frame_cap(&self, turn_number: u32) {
        let hidden = crate::memory_prefetch::maybe_enforce_frame_cap(
            &self.config,
            self.memory_router.as_deref(),
            turn_number,
        );
        // surface the cleanup so the user sees stale memory
        // being pruned (the "删除" half of their request).
        if hidden > 0 && !self.config.quiet_mode {
            self.emit_memory_update(&crate::memory_prefetch::MemoryWriteRecord {
                action: crate::memory_prefetch::MemoryAction::Cleanup,
                memory_type: crate::memory_prefetch::MemoryKind::Fact,
                key: "memory".to_string(),
                summary: format!("清理了 {hidden} 条过期/低权重记忆"),
                count: Some(hidden as u64),
            });
        }
    }

    pub(crate) fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        let records = crate::memory_prefetch::notify_memory_writes(
            &self.config,
            self.memory_router.as_deref(),
            tool_calls,
            results,
        );
        // surface explicit `memory` tool add/replace/remove
        // as precise write/update/delete chat rows.
        for rec in &records {
            self.emit_memory_update(rec);
        }
    }

    pub(crate) fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        let drained = crate::memory_prefetch::notify_delegations(
            &self.config,
            self.memory_router.as_ref(),
            &self.focus_stack,
            tool_calls,
            results,
            self.consciousness_tracker.as_ref(),
        );
        // surface delegation summaries (sub-task conclusions
        // archived to long-term memory) as "总结" chat rows.
        if drained > 0 && !self.config.quiet_mode {
            self.emit_memory_update(&crate::memory_prefetch::MemoryWriteRecord {
                action: crate::memory_prefetch::MemoryAction::Summarize,
                memory_type: crate::memory_prefetch::MemoryKind::Summary,
                key: "delegation".to_string(),
                summary: format!("归档了 {drained} 条子任务结论"),
                count: Some(drained as u64),
            });
        }
    }

    pub(crate) fn memory_on_turn_start(&self, turn: u32, message: &str) {
        crate::memory_prefetch::memory_on_turn_start(
            &self.config,
            self.memory_router.as_deref(),
            turn,
            message,
        );
    }

    /// (A): per-turn memory recall. Cheap heuristic
    /// scheduler that decides whether to emit a `memory_recall`
    /// event this turn (so the user sees "the agent thought of N
    /// related facts"), and updates the next-turn drift baseline.
    /// The caller (`run_loop::do_pre_turn_setup`) injects the
    /// rendered `RecallPlan` into message history as a system
    /// message via `ctx.add_message(Message::system(...))`, so the
    /// LLM can actually use the recalled facts. We also emit the
    /// visibility event so the user sees "thought of N items".
    /// Returns `Some(RecallPlan)` if a recall was emitted; the
    /// caller stores the keys in `state.last_recall_keys` for the
    /// next drift check.
    pub(crate) fn maybe_recall(
        &self,
        state: &LoopState,
        user_text: &str,
    ) -> Option<crate::recall_scheduler::RecallPlan> {
        if self.config.skip_memory || self.config.quiet_mode {
            return None;
        }
        let backend = self
            .memory_router
            .as_deref()
            .and_then(|r| r.find_embedded_backend());
        let last_keys_refs: Vec<&str> = state
            .last_recall_keys
            .iter()
            .map(|s| s.as_str())
            .collect();
        let inputs = crate::recall_scheduler::RecallInputs {
            turn_number: state.total_turns,
            user_text,
            last_recall_keys: &last_keys_refs,
            recall_interval: self.config.memory_flush_interval,
        };
        let trigger = crate::recall_scheduler::should_recall(&inputs);
        if trigger == crate::recall_scheduler::TriggerReason::None {
            return None;
        }
        let plan = match crate::recall_scheduler::gather_recall_items(backend.as_deref(), user_text, trigger) {
            Some(p) => p,
            None => return None,
        };

        // emit `memory_recall` so the user sees the
        // agent thought of something. The transport event carries
        // a small preview (top-3 keys + char count); the full plan
        // is only injected into the system prompt by
        // `build_system_prompt`, not by this path.
        if let Some(cb) = self.callbacks.status_callback.as_ref() {
            let preview_keys: Vec<&str> = plan
                .items
                .iter()
                .take(3)
                .map(|i| i.canonical_key.as_str())
                .collect();
            let payload = serde_json::json!({
                "trigger": match plan.triggered_by {
                    crate::recall_scheduler::TriggerReason::FirstTurn => "first_turn",
                    crate::recall_scheduler::TriggerReason::Interval => "interval",
                    crate::recall_scheduler::TriggerReason::Drift => "drift",
                    crate::recall_scheduler::TriggerReason::None => "none",
                },
                "count": plan.items.len(),
                "total_chars": plan.total_chars,
                "preview_keys": preview_keys,
                "turn_number": state.total_turns,
            });
            cb("memory_recall", &payload.to_string());
        }

        Some(plan)
    }

    pub(crate) fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        crate::memory_prefetch::memory_pre_compress_note(
            &self.config,
            self.memory_router.as_deref(),
            messages,
        )
    }

    pub(crate) fn auth_tokens_path(&self) -> PathBuf {
        crate::runtime_provider::auth_tokens_path(&self.config)
    }

    /// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    pub(crate) fn resolve_initial_system_prompt(
        &self,
        user_message: &str,
        tool_schemas: &[ToolSchema],
        tool_call_count: usize,
    ) -> (String, bool) {
        let snapshot = self.tool_error_counter.lock().ok().map(|c| c.snapshot());
        let context_epoch = self.context_epoch.load(std::sync::atomic::Ordering::SeqCst);
        // ★ a memory/skill/context-file store mutated since the
        // frozen `config.stored_system_prompt` was built (epoch mismatch),
        // so force a fresh build instead of reusing the stale frozen string.
        // `config` is owned and the run-loop is `&self`, so we cannot update
        // `stored_system_prompt` in place. Instead the rebuild re-reads the
        // Context layer from disk via the `PromptCache` epoch key; the stable
        // + volatile layers remain cache hits, so the only real cost is two
        // small file reads on the first turn after the mutation, then
        // negligible. The LLM provider still prefix-caches by prompt content,
        // so rebuilds across turns do not invalidate the upstream cache.
        let has_frozen_prompt = self
            .config
            .stored_system_prompt
            .as_ref()
            .map_or(false, |s| !s.trim().is_empty());
        let force_rebuild =
            has_frozen_prompt && self.stored_prompt_epoch.load(std::sync::atomic::Ordering::SeqCst) != context_epoch;
        crate::prompt_assembly::resolve_initial_system_prompt_with_tool_errors(
            &self.config,
            &self.prompt_cache,
            self.memory_router.as_deref(),
            &self.callbacks,
            user_message,
            tool_schemas,
            tool_call_count,
            snapshot.as_ref(),
            context_epoch,
            force_rebuild,
        )
    }

    /// wire: snapshot the running per-tool error counter for
    /// callers that want to compose the demotion block outside the
    /// normal `resolve_initial_system_prompt` path (e.g. when the
    /// `user_message` differs from a request body — tests, REPL).
    pub fn tool_error_snapshot(&self) -> std::collections::HashMap<String, u32> {
        self.tool_error_counter
            .lock()
            .map(|c| c.snapshot())
            .unwrap_or_default()
    }

    /// wire: drain a slice of fresh `ToolErrorRecord`s into
    /// the per-tool error counter so the next prompt carries the
    /// failure pattern in its `[Tool Failure Hints]`. Called from
    /// [`AgentLoop::finalize_agent_result`] on every termination,
    /// so all run-loop paths converge through this counter without
    /// any per-call-site wiring.
    pub(crate) fn drain_tool_errors_into_counter(
        &self,
        errors: &[hermix_core::ToolErrorRecord],
    ) {
        if errors.is_empty() {
            return;
        }
        if let Ok(mut counter) = self.tool_error_counter.lock() {
            for err in errors {
                counter.record_error(err.tool_name.as_str());
            }
        }
    }

    /// Compress when total chars exceed 80% of the context budget (Python auto-compaction).
    /// Uses the advanced LLM-powered compressor when available (set via
    /// `with_compressor`), falling back to the simple character-based
    /// compressor otherwise.
    /// before the LLM compressor replaces the middle
    /// of the conversation with a summary, we **archive** the same
    /// slice to disk via [`archive_context_section`]. The summary
    /// becomes lossy-by-design; the archive file is the lossless
    /// evidence the user can re-open from the chat UI. The archive
    /// refs are stashed in `last_archive_refs` for the frontend.
    /// also emit `compaction_status` snapshots at
    /// the pressure-detected and post-attempt boundaries so the
    /// `CompactionStatusBanner` can render in real time. We clone
    /// the snapshot before dropping the compressor lock so the
    /// callback can fire on the upstream IO task without holding
    /// the compressor's `MutexGuard`.
    pub(crate) async fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        crate::compression_ops::auto_compress_if_over_threshold(
            &self.config,
            &self.callbacks,
            self.compression.as_ref(),
            &self.last_archive_refs,
            &self.latest_compressed_summary,
            &self.latest_compression_archived_turns,
            &self.latest_compression_metrics,
            ctx,
            |msgs| self.memory_pre_compress_note(msgs),
        )
        .await
    }

    /// P1 : background-spawned streaming compression
    /// entry point. Replaces the synchronous
    /// `auto_compress_if_over_threshold` call from the preflight and
    /// `do_pre_turn_setup` paths when pressure is moderate
    /// (50%–85%). **Never blocks the agent loop on the LLM
    /// summary call**: the heavy work runs in a `tokio::spawn` task
    /// and the result is rolled in by
    /// [`poll_in_flight_compression`] on the next turn.
    /// Decision matrix:
    /// - pressure < 50% — no-op (return immediately).
    /// - pressure >= 50% and compressor wired and slot empty —
    /// `tokio::spawn` a full compress; the agent loop body
    /// returns immediately.
    /// - pressure >= 85% (red line) — fall through to the
    /// synchronous fast-compress path so the pressure drops
    /// before the next LLM call. Cancels any in-flight spawn
    /// by overwriting the slot with `None`.
    /// - pressure >= 50% and slot already populated — do not
    /// double-spawn. Fall through to the synchronous fast path
    /// only if we're at the red line; otherwise just return so
    /// the existing spawn can finish on its own.
    pub(crate) async fn maybe_spawn_compression(&self, ctx: &mut ContextManager) {
        let total_tokens = ctx.total_tokens();
        let max_c = ctx.max_context_tokens().max(1);
        let pressure = total_tokens as f64 / max_c as f64;
        let threshold = ctx.pressure_threshold_tokens();
        let Some(compressor) = self.compression.as_ref() else {
            // No compressor wired — fall through to the legacy
            // character-based fallback. This preserves the historical
            // contract for `LocalAgentService::spawn_agent_loop`
            // callers that don't construct an LLM compressor.
            if total_tokens > threshold {
                self.auto_compress_if_over_threshold(ctx).await;
            }
            return;
        };
        if total_tokens <= threshold {
            return;
        }

        // High-pressure red line — synchronous fast compress.
        // The synchronous path uses `compress_fast` which skips the
        // LLM summary call, so we never block the agent loop on an
        // LLM round-trip even in the synchronous fallback.
        if pressure >= crate::compression_ops::HIGH_PRESSURE_RED_LINE_RATIO {
            tracing::warn!(
                pressure_pct = (pressure * 100.0) as u32,
                "high-pressure red line — cancelling any in-flight spawn, running sync fast compress"
            );
            // Cancel any in-flight spawn: drop the JoinHandle. The
            // spawn task itself continues to run (its outcome is
            // simply discarded by the next poll). The synchronous
            // path below takes the compressor lock.
            if let Ok(mut slot) = self.compression_in_flight.lock() {
                *slot = None;
            }
            self.auto_compress_if_over_threshold(ctx).await;
            return;
        }

        // Moderate pressure — try to spawn. If a previous spawn is
        // still in flight, do nothing; the next
        // `poll_in_flight_compression` will roll it in.
        if !crate::compression_ops::in_flight_slot_is_empty(&self.compression_in_flight) {
            tracing::debug!(
                pressure_pct = (pressure * 100.0) as u32,
                "moderate pressure with in-flight spawn — skipping spawn, will poll next turn"
            );
            return;
        }
        let mut snapshot_messages = ctx.get_messages().to_vec();
        let snapshot_len = snapshot_messages.len();
        let snapshot_tokens = total_tokens;
        // Resolve the memory pre-compress note *here* on the main
        // loop body so the spawn task doesn't have to borrow
        // `self`. Mirrors the synchronous path in
        // `auto_compress_if_over_threshold` exactly.
        if let Some(note) = self.memory_pre_compress_note(&snapshot_messages) {
            snapshot_messages.push(Message::system(note));
        }
        let handle = crate::compression_ops::trigger_spawn_compression(
            &self.config,
            compressor.clone(),
            snapshot_messages,
            snapshot_len,
            snapshot_tokens,
            max_c,
            pressure,
            threshold as u64,
        );
        crate::compression_ops::install_in_flight(
            &self.compression_in_flight,
            crate::agent_loop::CompressionInFlight {
                handle,
                snapshot_len,
                snapshot_tokens,
                started_at: std::time::Instant::now(),
                max_context_tokens: max_c,
            },
        );
        tracing::info!(
            pressure_pct = (pressure * 100.0) as u32,
            snapshot_len,
            snapshot_tokens,
            "spawned background compression — agent loop will not block on LLM summary"
        );
    }

    /// P1 : poll the in-flight background compression
    /// (if any) and roll the outcome into `ctx` when the spawn has
    /// finished. Called at the top of every `do_pre_turn_setup` so
    /// the agent loop sees the compressed state by the time it
    /// builds the next LLM request.
    /// `wait_for_finish` controls whether we await the join handle
    /// with a timeout. The high-pressure red-line path
    /// ([`maybe_spawn_compression`]) sets it to `true` after
    /// cancelling the spawn, but in practice the high-pressure path
    /// just cancels and runs sync fast — `wait_for_finish = false`
    /// is the safe default.
    pub(crate) async fn poll_in_flight_compression(
        &self,
        ctx: &mut ContextManager,
        wait_for_finish: bool,
    ) -> bool {
        // The spawn task built a fully self-contained
        // [`crate::agent_loop::CompressionOutcome`] (including
        // summary / archived turns / engine metrics / post-snap).
        // The apply path inside `try_collect_in_flight` splices the
        // messages; this method forwards the rest of the outcome
        // into the agent loop's persistence-bound slots.
        // We do this by re-locking the compressor: the spawn task
        // already wrote the engine-side fields, and the apply path
        // is on the same thread as the agent loop body, so a brief
        // `try_lock` is enough.
        let applied = crate::compression_ops::try_collect_in_flight(
            &self.compression_in_flight,
            ctx,
            &self.last_archive_refs,
            wait_for_finish,
        )
        .await;
        if applied {
            self.refresh_post_compression_slots_from_outcome();
            self.emit_post_compression_status();
        }
        applied
    }
}
