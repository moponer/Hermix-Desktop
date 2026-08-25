//! Post-compression slot refresh and status emission.
use std::collections::HashMap;


use super::*;

impl AgentLoop {
    /// P1 : copy the engine-side compression state
    /// (summary / archived turns / metrics) into the agent loop's
    /// persistence-bound slots so `finalize_agent_result` can
    /// forward them to `local_agent_service`. The spawn task
    /// already wrote the engine fields; this method just snapshots
    /// them out into the `&Mutex` slots the persistence layer
    /// reads. `try_lock` is fine because the agent loop is
    /// single-threaded — the spawn task has finished and dropped
    /// its `tokio::sync::MutexGuard` by the time we get here.
    pub(crate) fn refresh_post_compression_slots_from_outcome(&self) {
        let Some(compressor) = self.compression.as_ref() else {
            return;
        };
        let guard = match compressor.try_lock() {
            Ok(g) => g,
            Err(_) => {
                tracing::debug!(
                    "refresh_post_compression_slots_from_outcome: \
                     compressor busy, deferring slot refresh"
                );
                return;
            }
        };
        if let Some(prev) = guard.previous_summary() {
            if let Ok(mut slot) = self.latest_compressed_summary.lock() {
                *slot = Some(prev.to_string());
            }
        }
        let archived = guard.last_archived_turns().to_vec();
        if !archived.is_empty() {
            if let Ok(mut slot) = self.latest_compression_archived_turns.lock() {
                *slot = Some(archived);
            }
        }
        if let Ok(mut slot) = self.latest_compression_metrics.lock() {
            let engine_metrics = guard.last_metrics();
            *slot = Some(crate::compression_ops::translate_engine_metrics(
                &engine_metrics,
                engine_metrics.trigger_reason.clone(),
            ));
        }
    }

    /// P1 : emit the `CompactionStatusBanner` post-snap
    /// the spawn task produced. The synchronous path emits it inline
    /// in `auto_compress_if_over_threshold`; the spawn path emits it
    /// here on the next `do_pre_turn_setup` when the outcome lands.
    /// The snapshot is already constructed and stored on the engine
    /// by the spawn task; we just forward it through the callback.
    pub(crate) fn emit_post_compression_status(&self) {
        let Some(compressor) = self.compression.as_ref() else {
            return;
        };
        let mut guard = match compressor.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // `breaker_snapshot` rebuilds from live engine state — the
        // metrics we passed in at spawn time are reflected here
        // because the spawn task already ran `record_outcome` on
        // the same engine.
        let tokens_saved = guard.last_metrics().tokens_saved;
        // derive `phase` from the engine's own metrics
        // — the spawn path doesn't get an explicit "complete"/"failed"
        // label from the caller, so we infer it. `summary_errors == 0`
        // means success (matches the post-snap logic in
        // `compression_ops::auto_compress_if_over_threshold`).
        let summary_errors = guard.last_metrics().summary_errors;
        let phase = if summary_errors == 0 { "complete" } else { "failed" };
        let snap = guard.breaker_snapshot_with_phase(tokens_saved, 0, 0, None, phase);
        crate::message_helpers::emit_compaction_status(&self.callbacks, &snap);
    }

    pub(crate) fn emit_status(&self, event_type: &str, message: &str) {
        crate::message_helpers::emit_status(&self.config, &self.callbacks, event_type, message)
    }

    /// emit a `usage_update` status event carrying the
    /// per-turn token breakdown (cache hits / cache writes /
    /// reasoning / prompt / completion) so the front-end cache-hit
    /// chip can update in real time. Bridges through the same
    /// `status_callback` path that `context_usage` uses, so the
    /// hermix-server handler in `lib.rs` can convert it to the
    /// `StreamEvent::UsageUpdate` variant without needing a new
    /// callback type.
    /// Honors `quiet_mode` like `emit_status` so log-replay jobs
    /// don't spam the WS stream with usage chips.
    pub(crate) fn emit_usage_update(&self, usage: &hermix_core::UsageStats) {
        if self.config.quiet_mode {
            return;
        }
        let payload = serde_json::json!({
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "cache_creation_tokens": usage.cache_creation_tokens,
            "reasoning_tokens": usage.reasoning_tokens,
        });
        if let Some(cb) = self.callbacks.status_callback.as_ref() {
            cb("usage_update", &payload.to_string());
        }
    }

    /// emit a `memory_update` status event carrying one
    /// memory mutation (write/update/delete/summarize/cleanup) so the
    /// front-end can render it inline in the conversation. Bridges
    /// through the same `status_callback` path that `usage_update`
    /// uses. Honors `quiet_mode` like the other emit helpers.
    pub(crate) fn emit_memory_update(&self, rec: &crate::memory_prefetch::MemoryWriteRecord) {
        if self.config.quiet_mode {
            return;
        }
        let payload = serde_json::json!({
            "action": rec.action.as_str(),
            "memory_type": rec.memory_type.as_str(),
            "key": rec.key,
            "summary": rec.summary,
            "count": rec.count,
        });
        if let Some(cb) = self.callbacks.status_callback.as_ref() {
            cb("memory_update", &payload.to_string());
        }
    }

    pub(crate) fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        crate::message_helpers::assistant_visible_text_after_think_blocks(m)
    }

    pub(crate) fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
        crate::message_helpers::normalize_tool_call_arguments(tc)
    }

    // -- Retry-aware LLM call ---------------------------------------------

    pub(crate) fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermix_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, route))
    }

    pub(crate) async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
    ) -> Result<hermix_core::LlmResponse, AgentError> {
        crate::llm_retry::call_llm_with_retry(
            &self.config,
            &self.oauth_refresh,
            &self.callbacks,
            &self.llm_provider,
            self.plugin_manager.as_ref(),
            self.primary_credential_pool.as_ref(),
            &self.interrupt,
            ctx,
            tool_schemas,
            route,
        )
        .await
    }

    /// Apply one streaming [`StreamChunk`] during [`Self::collect_stream_llm_response`].
    /// `think_parser` is a stateful splitter that pulls `<think>...</think>` text
    /// out of the visible content stream in real time and routes it to
    /// `reasoning_content` instead. Without this, providers that embed their
    /// reasoning in `delta.content` (MiniMax M2.7, Qwen, DeepSeek-R1) would
    /// leak the whole thinking monologue into the user-visible reply.
    /// `tool_call_sanitizer` is a stateful redactor that pulls raw
    /// `<minimax:tool_call>…</minimax:tool_call>` /
    /// `<invoke name="…">…</invoke>` blocks out of the *clean* (post-think)
    /// text. Without it, providers that emit tool calls as raw XML inside
    /// `delta.content` (MiniMax, older Hermes fine-tunes) would leak the
    /// whole XML envelope to the user. See
    /// `hermix_core::tool_call_sanitizer` for the full state machine.
    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
    pub(crate) async fn collect_stream_llm_response(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        active_model: &str,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<StreamCollectOutcome, AgentError> {
        crate::stream_collect::collect_stream_llm_response(
            &self.config,
            &self.oauth_refresh,
            &self.callbacks,
            &self.llm_provider,
            self.primary_credential_pool.as_ref(),
            &self.interrupt,
            ctx,
            tool_schemas,
            route,
            active_model,
            on_chunk,
        )
        .await
    }
    pub(crate) async fn prepare_run_context(
        &self,
        mut messages: Vec<Message>,
        tools: Option<&Vec<ToolSchema>>,
    ) -> PreparedRun {
        let mut ctx = ContextManager::for_model_with_providers(
            &self.config.model,
            Some(&self.config.llm_providers),
        );
        // Apply user-configured pressure ratio from compaction config.
        if let Some(ratio) = self.config.context_pressure_ratio {
            ctx = ctx.with_pressure_ratio(ratio);
        }

        // Sanitize surrogates and strip prior budget warnings from history
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        //Determine which tools to expose ( H1: route through tool_router
        // when configured; otherwise send the full registry as before).
        let tool_schemas: Vec<ToolSchema> = match tools {
            Some(t) => self.route_tools(t.clone(), &messages),
            None => {
                let all = self.tool_registry.schemas();
                self.route_tools(all, &messages)
            }
        };
        // Log the *resolved* tool count (after routing), not the raw
        // parameter. The `tool_count` on the span is the caller's
        // argument and is zero when the caller passes `None`. Was
        // previously open-coded in `run_stream` only; centralizing
        // here so both `run` and `run_stream` emit the same diagnostic.
        if tool_schemas.is_empty() {
            tracing::warn!(
                registry_size = self.tool_registry.schemas().len(),
                "No tool schemas after routing — LLM will not see any tools"
            );
        } else {
            tracing::info!(
                resolved = tool_schemas.len(),
                "Tool schemas resolved for this turn"
            );
        }

        // Build and inject system prompt (or reuse SQLite-cached prompt for session continuity)
        let tool_call_count = ctx.get_messages()
            .iter()
            .filter(|m| m.tool_calls.is_some())
            .count();
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas, tool_call_count);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .rfind(|(_, m)| m.role == MessageRole::User)
                .map(|(i, _)| i)
        } else {
            None
        };
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut codex_ack_continuations: u32 = 0;
        // Counts how many times the autonomous loop has re-driven the LLM after
        // it produced plain text without tool calls. Capped at
        // `default_max_autonomous_nudges()` to prevent infinite loops when the
        // model genuinely cannot make further progress.
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut autonomous_nudge_count: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch for first user message
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let first_user = ctx
            .get_messages()
            .iter()
            .rfind(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id, Some(ctx.get_messages())).await;
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        //(BaiLongma gatherer): pre-fetch file context referenced
        // in the user message so the LLM doesn't need to call read_file
        // on the first turn. Best-effort; failures are silently ignored.
        if !first_user.is_empty() {
            let gathered = crate::gatherer::gather_context(
                &first_user,
                self.memory_router.as_ref(),
                None,
            )
            .await;
            if let Some(gather_msg) = crate::gatherer::format_gathered_context(&gathered) {
                ctx.add_message(gather_msg);
            }
        }

        if self.config.preflight_context_compress {
            //P1 : background-spawned compression.
            // `maybe_spawn_compression` decides between spawn (no
            // block), sync fast (>=85% red line), or no-op (under
            // threshold). The preflight path used to block on the
            // LLM summary call here; with P1 the worst case is the
            // synchronous fast-compress fallback, which itself
            // skips the LLM. After this call the next turn's
            // `do_pre_turn_setup` will poll the in-flight handle
            // and roll the result in.
            self.maybe_spawn_compression(&mut ctx).await;
        }

        PreparedRun {
            ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            codex_ack_continuations,
            autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        }
    }


    /// Run the agent loop with streaming.
    /// Uses the LLM provider's streaming API and invokes `on_chunk` for each
    /// incremental delta. The stream is collected into a complete response
    /// before tool execution proceeds.
    /// Remove duplicate tool calls that share the same function name and arguments.
    pub(crate) fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        crate::tool_dispatch::deduplicate_tool_calls(calls)
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    pub(crate) fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        crate::tool_dispatch::repair_tool_call(&self.tool_registry, tc)
    }

    /// Inject current session id into `session_search` calls when absent.
    pub(crate) fn hydrate_session_search_args(&self, tc: &mut ToolCall) {
        crate::tool_dispatch::hydrate_session_search_args(self.config.session_id.as_deref(), tc)
    }

    pub(crate) fn turn_route_cost_guard(&self, model: String) -> TurnRuntimeRoute {
        crate::cost_routing::turn_route_cost_guard(
            &self.config,
            self.primary_credential_pool.as_ref(),
            model,
        )
    }

    pub(crate) fn resolve_smart_runtime_route(&self, messages: &[Message]) -> Option<TurnRuntimeRoute> {
        crate::cost_routing::resolve_smart_runtime_route(
            &self.config,
            &self.oauth_refresh,
            self.primary_credential_pool.as_ref(),
            messages,
        )
    }

    pub(crate) fn resolve_cost_degrade_model(&self) -> Option<String> {
        crate::cost_routing::resolve_cost_degrade_model(&self.config)
    }

    /// Ask the LLM for a progress summary when the soft turn budget is
    /// exhausted. Uses the conversation's own `active_model` (the same
    /// one the main loop resolves for the LLM call) — the summary must
    /// run on the model the user picked in the dialog, NOT on a cheap
    /// router fallback that can resolve to an un-normalized model string
    /// (e.g. `minimax:minimax-m3`) and 400 on strict providers.
    pub(crate) async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
        active_model: &str,
    ) -> Result<Option<Message>, AgentError> {
        crate::session_lifecycle::handle_max_iterations(
            &self.config,
            &self.llm_provider,
            ctx,
            active_model,
        )
        .await
    }

    /// Build a per-tool hint that we append to a user-denial reason so the
    /// LLM, on its next turn, has a concrete suggestion for *which* tool
    /// would have been more appropriate.
    /// ## Why this exists
    /// Regression : user said "看下 D:\data 下有啥". The model
    /// chose `computer_use` with `action=screenshot`. The user denied.
    /// The model then claimed "I have no terminal / list_files / read_file
    /// tool" — even though all three are registered and pinned to the top
    /// of the `<tools>` block.
    /// Two layers of defense had been added (descriptions + system-prompt
    /// reminder) but neither was anchored to the deny path. By the time
    /// the user hits Deny, the model has already committed to its choice;
    /// a plain "User denied the tool call." gives it no signal to re-pick.
    /// The hint below is the *third* defense: when the user rejects,
    /// surface the right next move in the very tool-result message the
    /// model will read.
    /// ## Design constraints
    /// - **Short** — the tool-result string is fed back to the LLM, and
    /// every extra token here competes with the user's original prompt
    /// in the next turn. We aim for one sentence, not a paragraph.
    /// - **Tool-specific** — generic "consider another tool" hints are
    /// too vague. We map the most-mis-picked tools to the right
    /// replacement so the LLM has a concrete noun to call.
    /// Resolve the pre-tool-dispatch hook for every tool call up front.
    /// When `AgentCallbacks::on_pre_tool_dispatch` is set, this invokes the
    /// hook once per tool call. `Allow` and `Deny` decisions resolve
    /// synchronously; `AwaitApproval` blocks on a `oneshot::Receiver` until
    /// the upstream layer (e.g. `LocalAgentService`) sends a verdict. The
    /// returned map is keyed by `ToolCall::id` and consumed by both the
    /// orchestrated `delegate_task` path and the `JoinSet` path.
    pub(crate) async fn resolve_pre_dispatch(
        &self,
        tool_calls: &[ToolCall],
    ) -> HashMap<String, Result<(), crate::tool_dispatch::ToolDenial>> {
        crate::tool_dispatch::resolve_pre_dispatch(
            self.callbacks.on_pre_tool_dispatch.as_ref().map(|b| b.as_ref()),
            tool_calls,
            self.callbacks.shutdown.as_ref(),
            self.callbacks.on_pre_tool_approved.as_ref(),
        )
        .await
    }

    pub(crate) fn resolve_max_delegate_depth(&self) -> u32 {
        crate::tool_dispatch::resolve_max_delegate_depth()
    }

    /// Cap concurrent delegate_task calls based on config.
    pub(crate) fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
        crate::tool_dispatch::cap_delegates(self.config.max_concurrent_delegates, tool_calls)
    }

    pub(crate) fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config.background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermix_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// terminal-persistence shared by `run` and `run_stream`
    /// natural-finish paths. Extracted to keep the two call sites in
    /// lock-step (P3 added `maybe_freeze_snapshot` to run_stream and we
    /// don't want that fix to drift again).
    pub(crate) fn persist_session_terminal(
        &self,
        ctx: &ContextManager,
        session_id: &str,
        total_turns: u32,
        review_memory_at_end: bool,
    ) {
        let (u, a) = extract_last_user_assistant(ctx.get_messages());
        self.memory_sync(&u, &a, session_id);
        //freeze a final snapshot at session end so the next
        // session can pick up without replaying every frame.
        self.maybe_freeze_snapshot(session_id, total_turns);
        self.spawn_background_review(total_turns, ctx, review_memory_at_end);
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    pub(crate) fn spawn_background_review(&self, turn: u32, ctx: &ContextManager, review_memory_at_end: bool) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config.background_review_enabled {
            return;
        }
        let mut review_skills = false;
        //only fire the skill review when the user hasn't
        // opted out via `auto_skill_extraction: false` in
        // ~/.hermix/config.yaml. Without this guard, the master
        // off-switch would be ignored by the background loop.
        if self.config.auto_skill_extraction
            && self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                if c.iters_since_skill >= self.config.skill_creation_nudge_interval {
                    review_skills = true;
                    c.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = self.config.clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        //child session must NOT write skills back to the
        // user's primary skill library — the background-review
        // task is itself a child of the parent's agent loop, and
        // a misbehaving reflection would pollute
        // `~/.hermix/skills/`. Skill creation only happens at the
        // top-level agent.
        cfg.auto_skill_extraction = false;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.max_turns = cfg.max_turns.min(8);
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let review_cb = self.callbacks.background_review_callback.clone();
        let handle = tokio::spawn(async move {
            let agent = AgentLoop::new(cfg, tools, provider);
            match agent.run(hist, None).await {
                Ok(result) => {
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summarize_background_review_result(&result.messages)
                        {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
        // Store the JoinHandle for graceful shutdown
        if let Ok(mut tasks) = self.background_tasks.lock() {
            tasks.push(handle);
        }
    }

    /// Recover todo-state hints from historical messages at loop start.
    pub(crate) fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }

    /// See [`PostLlmAction`]. Mutates `inner_thinking` / `inner_empty`
    /// when bumping the per-attempt retry budget and emits a `lifecycle`
    /// status line on each transition.
    pub(crate) fn classify_post_llm_response(
        &self,
        r: hermix_core::LlmResponse,
        inner_thinking: &mut u32,
        inner_empty: &mut u32,
        ctx: &mut ContextManager,
    ) -> PostLlmAction {
        crate::session_lifecycle::classify_post_llm_response(
            &self.config,
            &self.callbacks,
            r,
            inner_thinking,
            inner_empty,
            ctx,
        )
    }
}
