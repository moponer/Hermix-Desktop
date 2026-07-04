use std::sync::atomic::Ordering;
use crate::budget;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::time::sleep;
use tracing::{info, instrument};
use hermix_core::{AgentError,AgentResult,format_token_count,LlmResponse,Message,MessageRole,StreamChunk,ToolCall,ToolErrorRecord,ToolSchema,UsageStats};
use crate::agent_helpers::default_max_autonomous_nudges;
use crate::agent_loop::{
    AgentLoop, LoopState, PostLlmAction, PostResponseAction, PreparedRun, TurnRuntimeRoute,
    extract_last_user_assistant, extract_file_change,
};
use crate::context::ContextManager;
use crate::cost_guard::merge_usage;
use crate::plugins::HookType;
use crate::python_alignment::{budget_pressure_text, inject_budget_pressure_into_last_tool_result};
use crate::stream_collect::StreamCollectOutcome;
use crate::message_helpers::assistant_visible_text_after_think_blocks;
use crate::tool_result_processor::push_tool_result;

impl AgentLoop {
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        // ---- Step 1: Prepare the run context ----
        // Sanitize messages, route tools, build the system prompt, hydrate the
        // todo store, prefetch memory, and run the optional preflight compress.
        // All of this is the "before any model call" work and is a pure
        // function of the inputs (no streaming, no tool calls yet).
        #[allow(unused_mut)] // codex_ack_continuations is read-only in run() today but mut is fine for future edits
        let PreparedRun {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        } = self.prepare_run_context(messages, tools.as_ref()).await;

        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();

        // Phase 1.1 (2026-07-03) introduced `state` as a bridge — every
        // loop-local mutable would eventually live in `state` so a future
        // `run_shared_loop(state, llm_caller)` helper could take only
        // `&mut LoopState` instead of the original 11+ locals.
        //
        // Phase 5 (2026-07-03..2026-07-04) collapsed the bridge to its
        // minimum: 4 manager structs (`cost_guard` / `retry_counters` /
        // `context_pressure` / `timings` / `message_cache`) own the
        // in-place-mutated fields, and `total_turns` is the only scalar
        // still aliased to a local. The other 13 locals were either
        // dropped or replaced with `record_*` / `bump_*` / `store_*`
        // methods on the inner structs.
        let mut state = LoopState::default();
        // `total_turns` keeps the local-alias pattern because the rest
        // of the loop body reads it as a scalar (the hot path is
        // `if total_turns >= self.config.max_turns` etc.). The struct
        // field `state.total_turns` is still written at the end of
        // each iteration so the future shared loop body can read it
        // without taking 11 more arguments.
        let mut total_turns: u32 = state.total_turns;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            // `/steer` pre-API drain — inject into last tool result before the next LLM call.
            self.pre_api_apply_pending_steer(&mut ctx);

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    None,
                ));
            }

            total_turns += 1;
            tracing::debug!("Agent turn {}", total_turns);

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.oauth_refresh
                .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
                .await;

            // Skill nudge counter — Python `run_agent.py`: increment at the start of each inner API iteration.
            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
            {
                // Phase 5 (2026-07-04): store checkpoint directly on
                // the `state.message_cache` (no more local alias).
                state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
            }

            // Notify memory + plugins of new turn
            self.memory_on_turn_start(total_turns, "");

            // Memory review hint: every 20 turns, remind LLM to consolidate.
            if total_turns > 0 && total_turns.is_multiple_of(20) {
                let review_hint = "Hint: Review and consolidate your recent memory entries. \
                    Remove redundant or outdated information using the memory tool.";
                ctx.add_message(Message::system(review_hint));
            }

            // Memory sync at flush interval
            if total_turns.is_multiple_of(self.config.memory_flush_interval) && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id);
            }
            // Phase 18 (架构-1 + 架构-2): background snapshot + cap.
            self.maybe_freeze_snapshot(session_id, total_turns);
            self.maybe_enforce_frame_cap(total_turns);

            // --- Pre-LLM hook ---
            let turn_runtime_route = state.route_state.forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Prefetch: identify user intent, run safe tools, inject results as system prompt ---
            if let Some(ref prefetcher) = self.prefetcher {
                let tool_names: Vec<String> = tool_schemas.iter().map(|t| t.name.clone()).collect();
                let user_text: String = ctx
                    .get_messages()
                    .iter()
                    .filter(|m| m.role == MessageRole::User)
                    .filter_map(|m| m.content.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                let recent_msgs: Vec<Message> = ctx
                    .get_messages()
                    .iter()
                    .cloned()
                    .rev()
                    .take(10)
                    .collect();
                let tc = hermix_intelligence::TurnContext::from_user_message(user_text.clone())
                    .with_recent_messages(recent_msgs);
                if let Some(ref outcome) = prefetcher.run(&tc, &tool_names).await {
                    let block = outcome.to_system_prompt_block();
                    if !block.is_empty() {
                        ctx.add_message(Message::system(&block));
                        tracing::trace!(intent = ?outcome.intent, confidence = outcome.confidence, "prefetch result injected as system prompt block");
                    }
                    // H3: forward prefetch intent to UI layer
                    if let Some(ref on_prefetch_intent) = self.callbacks.on_prefetch_intent {
                        let json = serde_json::to_string(&json!({
                            "intent": serde_json::to_value(outcome.intent.clone()).unwrap_or_default(),
                            "confidence": outcome.confidence,
                            "results": outcome.results.iter().map(|r| json!({
                                "tool": &r.tool,
                                "args": &r.args,
                                "output": &r.output,
                                "errored": r.errored,
                                "duration_ms": r.duration_ms,
                            })).collect::<Value>(),
                        }));
                        if let Ok(ref payload) = json {
                            on_prefetch_intent(payload);
                        }
                    }
                }

                // L4: dual-layer thinking — pick L1 (Quick) or L2 (Deep)
                // for this turn. The mode is informed by the prefetch
                // outcome (intent) and the user text. We only emit a
                // tracing event + ACUI directive here; the actual prompt
                // branching (when to use L1 vs L2 prefix) is the
                // responsibility of the prompt builder that consumes
                // this decision.
                let thinking_decision = {
                    use hermix_intelligence::{select_mode, ThinkingConfig};
                    let cfg = ThinkingConfig::default();
                    let history_len = ctx.get_messages().len();
                    select_mode(user_text.as_str(), history_len, &cfg)
                };
                tracing::trace!(
                    mode = thinking_decision.mode.as_str(),
                    reason = thinking_decision.reason.as_str(),
                    confidence = thinking_decision.confidence,
                    "L4 thinking mode selected for this turn"
                );
                if let Some(ref on_acui) = self.callbacks.on_acui_directive {
                    let directive = hermix_core::AcuiDirective::Show(
                        hermix_core::AcuiShow::new(
                            format!("thinking-mode-{}", total_turns),
                            "thinking_mode",
                            serde_json::json!({
                                "mode": thinking_decision.mode.as_str(),
                                "reason": thinking_decision.reason.as_str(),
                                "confidence": thinking_decision.confidence,
                            }),
                        ),
                    );
                    if let Ok(json) = serde_json::to_string(&directive) {
                        on_acui(&json);
                    }
                }
            }

            // --- LLM API call with transport retry + semantic empty/thinking recovery (Python parity) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = match self
                    .call_llm_with_retry(&ctx, &tool_schemas, turn_runtime_route.as_ref())
                    .await
                {
                    Ok(r) => r,
                    Err(AgentError::Interrupted { .. }) => {
                        return Ok(self.graceful_interrupt_result(
                            &ctx,
                            total_turns,
                            &tool_errors,
                            state.cost_guard.accumulated_usage.clone(),
                            state.cost_guard.session_cost_usd,
                            session_started_hooks_fired,
                            persist_user_idx,
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // Phase 4.2 (2026-07-03): ~40 lines of has_tools /
                // visible_text / reasoning-retry / empty-retry /
                // exhausted classification now lives in
                // `classify_post_llm_response` (Phase 4.1). The
                // per-attempt state (`inner_thinking` / `inner_empty`)
                // is owned by the loop and threaded through.
                match self.classify_post_llm_response(
                    r,
                    &mut inner_thinking,
                    &mut inner_empty,
                    &mut ctx,
                ) {
                    PostLlmAction::Break(resp) => break resp,
                    PostLlmAction::RetryReasoning | PostLlmAction::RetryEmpty => continue,
                }
            };
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            // Phase 5 (2026-07-04): record timings on `state.timings`
            // directly (saturating-add inside) instead of accumulating
            // to a local and bridging back via the now-removed
            // `publish_from` helper.
            state.timings.record_api(api_elapsed);

            // Phase 1.3b (2026-07-03): post-LLM hook + usage accumulate +
            // cost guard extracted into `process_post_response`. ~60 lines
            // of duplication with `run_stream()` collapsed into one helper
            // call. Only the `CostGuardTripped` variant short-circuits the
            // loop here; `Finalize` (no tool calls) falls through to the
            // existing `decide_empty_response` block below.
            if matches!(
                self.process_post_response(
                    &mut ctx,
                    &response,
                    turn_usage_acc.as_ref(),
                    total_turns,
                    api_elapsed,
                    &mut state.cost_guard,
                    &mut state.route_state,
                    &mut state.message_cache,
                    false, // stream_mode
                )?,
                PostResponseAction::CostGuardTripped
            ) {
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    None,
                ));
            }

            let (history_includes_tool, assistant_msg, tool_calls) = self.prepare_assistant_turn(
                &mut ctx,
                &response,
                &mut state.message_cache,
            );

            // Step complete callback
            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done. The fallback rules
            // (Codex ack continuation / autonomous nudge / patch last
            // assistant content / finalize) used to be open-coded here
            // and duplicated in `run()`. C2 consolidated that logic
            // into `crate::empty_response_handler::decide_empty_response`
            // so both call sites use the same single authority.
            let tool_calls = match tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    let outcome = crate::empty_response_handler::decide_empty_response(
                        crate::empty_response_handler::EmptyResponseInput {
                            assistant_msg: &assistant_msg,
                            api_mode: self.config.api_mode.clone(),
                            has_tool_schemas: !tool_schemas.is_empty(),
                            history_includes_tool,
                            task_hint: &task_hint,
                            codex_ack_continuations,
                            autonomous_mode: self.config.autonomous_mode,
                            autonomous_nudge_count,
                            total_turns,
                            max_turns: self.config.max_turns,
                            max_autonomous_nudges: default_max_autonomous_nudges(),
                            last_content_with_tools: state.message_cache.take_content_with_tools(),
                            assistant_has_visible_text:
                                Self::assistant_visible_text_after_think_blocks,
                        },
                    );
                    codex_ack_continuations = outcome.new_codex_ack_continuations;
                    autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                    if self.apply_fallback_outcome(
                        &mut ctx,
                        outcome.action,
                        false, // stream_mode
                        total_turns,
                        autonomous_nudge_count,
                    ) {
                        continue;
                    }
                    tracing::debug!("No tool calls in response, finishing naturally");
                    // Phase 2.4: terminal-persistence shared with run_stream
                    self.persist_session_terminal(&ctx, session_id, total_turns, review_memory_at_end);
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        true,
                        false,
                        None,
                    ));
                }
            };

            codex_ack_continuations = 0;

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                let count = state.retry_counters.bump_invalid_tool();
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        count, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if count >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        false,
                    None,
                    ));
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    push_tool_result(&mut ctx, &tool_calls, &tc.id, &content);
                }
                continue;
            }
            state.retry_counters.reset_invalid_tool();

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                let count = state.retry_counters.bump_invalid_json();
                if count < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            count, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                state.retry_counters.reset_invalid_json();
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    push_tool_result(&mut ctx, &tool_calls, &tc.id, &content);
                }
                continue;
            }
            state.retry_counters.reset_invalid_json();

            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }

            // Cap concurrent delegate_task calls
            self.cap_delegates(&mut tool_calls);

            // --- Pre-tool hook ---
            //
            // Generate a per-invocation uuid up-front and stash it on
            // a side-table keyed by the LLM-assigned `tc.id`. We need
            // the same id back in the post-tool loop below so callers
            // (e.g. `LocalAgentService`'s `on_tool_complete` closure)
            // can pair start and complete events for the same tool
            // call. The `tc.id` is the only thing that survives both
            // the pre-loop (which sees `tool_calls`) and the
            // post-loop (which sees `results` — keyed by
            // `result.tool_call_id`, identical to `tc.id`).
            let mut tool_call_ids: HashMap<String, String> = HashMap::new();
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let tool_call_id = uuid::Uuid::new_v4().to_string();
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }
            let tool_start = Instant::now();
            let results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            // Phase 5 (2026-07-04): see `record_api` above.
            state.timings.record_tool(tool_elapsed);

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // --- Post-tool hook ---
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "is_error": res.is_error,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    // Fall back to a synthetic id if the pre-loop
                    // somehow didn't seed one (e.g. a tool call
                    // skipped by `cap_delegates` after the
                    // map was built). In practice this branch
                    // is never hit — every pre-loop `tc` gets
                    // a uuid — but keeping the fallback means a
                    // missing map entry doesn't blow up the
                    // dispatch.
                    let tool_call_id = tool_call_ids
                        .get(&tc.id)
                        .cloned()
                        .unwrap_or_else(|| format!("legacy-{}", tc.function.name));
                    cb(&tool_call_id, &tc.function.name, &res.content);
                }
            }

            // --- File-change summary events ---
            // Emit a `file_change` timeline event for tools that mutate
            // the filesystem (write_file, patch, delete). The chat UI
            // renders these as a WorkBuddy-style "文件变更 (N个)" row.
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                if res.is_error {
                    continue;
                }
                if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                    if let Some(ref cb) = self.callbacks.on_tool_complete {
                        let synth_id = format!("file_change::{}", change.path);
                        let synth_name = format!("file_change::{}", change.path);
                        let payload = serde_json::json!({
                            "change_type": change.change_type,
                            "file_path": change.path,
                            "file_summary": change.summary,
                        }).to_string();
                        cb(&synth_id, &synth_name, &payload);
                    }
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            // Enforce budget on tool results
            let mut results = results;
            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }

            for result in results {
                // Look up the original tool name (e.g. "terminal")
                // from the matching tool_calls entry so reloaded
                // sessions can rebuild ToolMessageMeta with the real
                // label. Without this the persisted `Message::name`
                // is None and the UI falls back to a generic "tool"
                // pill on reload.
                push_tool_result(&mut ctx, &tool_calls, &result.tool_call_id, &result.content);
            }
            self.post_tool_apply_pending_steer(&mut ctx);
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_tokens = ctx.total_tokens();
            let threshold = ctx.pressure_threshold_tokens();
            if threshold > 0 {
                let progress = total_tokens as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if state.context_pressure.should_emit_warning(progress, tier) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        format_token_count(total_tokens),
                        format_token_count(threshold)
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx).await;

            // TODO(2026-07-04): `total_turns` is aliased into both a
            // local and `state.total_turns` (dual-source). The
            // single-source fix is to extract `run_shared_loop(state,
            // llm_caller)` once both call sites share the same
            // continuation shape — at that point the local disappears
            // and the writeback goes away. Until then this writeback
            // bridges the value into the manager struct so callers that
            // only see `LoopState` (post-run) read a fresh value. See
            // the bridge comment at the top of `run()` for the full
            // history of the 14 -> 1 writeback shrink.
            state.total_turns = total_turns;
        }
    }

    #[instrument(
        name = "run_stream",
        level = "info",
        skip_all,
        fields(
            model = %self.config.model,
            provider = %self.config.provider.as_deref().unwrap_or(""),
            msg_count = messages.len(),
            tool_count = tools.as_ref().map(|t| t.len()).unwrap_or(0),
            total_turns = tracing::field::Empty,
            total_ms = tracing::field::Empty,
        )
    )]
    pub async fn run_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
        on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Result<AgentResult, AgentError> {
        let _run_started = Instant::now();
        info!(
            msg_count = messages.len(),
            tool_count = tools.as_ref().map(|t| t.len()).unwrap_or(0),
            "run_stream entered"
        );
        let on_chunk = match on_chunk {
            Some(cb) => cb,
            None => {
                return self.run(messages, tools).await;
            }
        };
        // Track the most recent finish_reason across all LLM calls in this
        // streaming run so we can surface it in AgentResult → WS Done event.
        let mut last_finish_reason: Option<String> = None;
        let stream_mute = Arc::new(AtomicBool::new(false));
        let stream_needs_break = Arc::new(AtomicBool::new(false));
        let raw_emit = on_chunk;
        let mute_for_emit = stream_mute.clone();
        let break_for_emit = stream_needs_break.clone();
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> =
            Box::new(move |chunk: StreamChunk| {
                let StreamChunk {
                    delta,
                    finish_reason,
                    usage,
                } = chunk;
                if let Some(delta_val) = delta {
                    if let Some(content) = delta_val.content.clone() {
                        if mute_for_emit.load(Ordering::Acquire) {
                            return;
                        }
                        let mut out = content;
                        if break_for_emit.swap(false, Ordering::AcqRel) {
                            out = format!("\n\n{}", out);
                        }
                        raw_emit(StreamChunk {
                            delta: Some(hermix_core::StreamDelta {
                                content: Some(out),
                                tool_calls: delta_val.tool_calls,
                                extra: delta_val.extra,
                            }),
                            finish_reason,
                            usage,
                        });
                        return;
                    }
                    raw_emit(StreamChunk {
                        delta: Some(delta_val),
                        finish_reason,
                        usage,
                    });
                    return;
                }
                raw_emit(StreamChunk {
                    delta: None,
                    finish_reason,
                    usage,
                });
            });

        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();
        // Phase 3.1 (2026-07-03): `run_stream` setup was an open-coded
        // re-implementation of `prepare_run_context`. The two functions
        // diverge only in their *non-prepare* concerns — `run()` is
        // non-stream, `run_stream()` wraps the chunk callback and
        // tracks `last_finish_reason`. The "before any model call"
        // work (sanitize / strip / task_hint / route_tools /
        // system_prompt / session_start hook / hydrate_todo /
        // persist_user_idx / codex+autonomous counters /
        // review_memory_at_end / memory_prefetch /
        // auto_compress_if_over_threshold) is identical and now
        // lives in one place. The tool-schemas tracing that used to
        // live only in `run_stream` was promoted to
        // `prepare_run_context` so both call sites emit the same
        // diagnostic.
        let PreparedRun {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        } = self.prepare_run_context(messages, tools.as_ref()).await;
        let session_id = self.config.session_id.as_deref().unwrap_or("");

        let mut state = LoopState::default();
        // Phase 5 (2026-07-04): same as in `run()`. Only `total_turns`
        // keeps the local-alias pattern. The two message caches live
        // on `state.message_cache` (mutated in place) and the two
        // timings live on `state.timings` (mutated in place).
        let mut total_turns: u32 = state.total_turns;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            // `/steer` pre-API drain — inject into last tool result before the next LLM call.
            self.pre_api_apply_pending_steer(&mut ctx);

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            total_turns += 1;

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.oauth_refresh
                .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
                .await;

            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            self.memory_on_turn_start(total_turns, "");

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
            {
                // Phase 5 (2026-07-04): store checkpoint directly on
                // the `state.message_cache` (no more local alias).
                state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
            }

            if total_turns.is_multiple_of(self.config.memory_flush_interval) && total_turns > 0 {
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
            }
            // Phase 18 (架构-1 + 架构-2): background snapshot + cap.
            self.maybe_freeze_snapshot(session_id, total_turns);
            self.maybe_enforce_frame_cap(total_turns);

            // Pre-LLM hook
            let turn_runtime_route = state.route_state.forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Streaming first attempt + same D-step semantic recovery as `run()` (retries use non-stream) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let mut inner_attempt: u32 = 0;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = if inner_attempt == 0 {
                    let pre_api_ctx = serde_json::json!({
                        "attempt": inner_attempt,
                        "model": active_model,
                        "stream": true,
                        "route_label": turn_runtime_route
                            .as_ref()
                            .and_then(|r| r.route_label.as_deref()),
                    });
                    let _ = self.invoke_hook(HookType::PreApiRequest, &pre_api_ctx);
                    let stream_collect = self
                        .collect_stream_llm_response(
                            &ctx,
                            &tool_schemas,
                            turn_runtime_route.as_ref(),
                            active_model,
                            &*on_chunk,
                        )
                        .await;
                    let post_api_ctx = match &stream_collect {
                        Ok(StreamCollectOutcome::Complete(resp)) => {
                            // Remember the most recent finish_reason so the
                            // final AgentResult can surface it to the WS client.
                            last_finish_reason = resp.finish_reason.clone();
                            serde_json::json!({
                                "attempt": inner_attempt,
                                "model": active_model,
                                "stream": true,
                                "ok": true,
                                "finish_reason": resp.finish_reason,
                            })
                        },
                        Ok(StreamCollectOutcome::Interrupted(_)) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": active_model,
                            "stream": true,
                            "ok": false,
                            "interrupted": true,
                        }),
                        Err(err) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": active_model,
                            "stream": true,
                            "ok": false,
                            "error": err.to_string(),
                        }),
                    };
                    let _ = self.invoke_hook(HookType::PostApiRequest, &post_api_ctx);
                    match stream_collect? {
                        StreamCollectOutcome::Complete(resp) => resp,
                        StreamCollectOutcome::Interrupted(partial) => {
                            state
                                .cost_guard
                                .record_usage(partial.usage.as_ref(), partial.model.as_str(), &self.config);
                            ctx.add_message(partial.message);
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                    }
                } else {
                    match self
                        .call_llm_with_retry(&ctx, &tool_schemas, turn_runtime_route.as_ref())
                        .await
                    {
                        Ok(r) => r,
                        Err(AgentError::Interrupted { .. }) => {
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                };
                inner_attempt = inner_attempt.saturating_add(1);

                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // Phase 4.2 (2026-07-03): mirrors the run() side —
                // ~40 lines of has_tools / visible_text /
                // reasoning-retry / empty-retry / exhausted
                // classification collapse into a single `match` on
                // `classify_post_llm_response` (Phase 4.1). The
                // `inner_attempt` counter (stream-only) is bumped
                // outside this match; per-attempt state is owned by
                // the loop.
                match self.classify_post_llm_response(
                    r,
                    &mut inner_thinking,
                    &mut inner_empty,
                    &mut ctx,
                ) {
                    PostLlmAction::Break(resp) => break resp,
                    PostLlmAction::RetryReasoning | PostLlmAction::RetryEmpty => continue,
                }
            };
            let api_elapsed_ms = api_start.elapsed().as_millis() as u64;

            // Phase 1.3c (2026-07-03): mirrors 1.3b on the run_stream()
            // side. Post-LLM hook + usage accumulate + cost guard move
            // into `process_post_response`. The stream-specific
            // `llm_response_received` info! line is now emitted inside
            // the helper when `stream_mode = true`. The
            // `last_finish_reason` was already updated inside the
            // stream retry loop (line ~5640) to the final response's
            // finish_reason — no need to refresh it here.
            if matches!(
                self.process_post_response(
                    &mut ctx,
                    &response,
                    turn_usage_acc.as_ref(),
                    total_turns,
                    api_elapsed_ms,
                    &mut state.cost_guard,
                    &mut state.route_state,
                    &mut state.message_cache,
                    true, // stream_mode
                )?,
                PostResponseAction::CostGuardTripped
            ) {
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            let (history_includes_tool, assistant_msg, tool_calls) = self.prepare_assistant_turn(
                &mut ctx,
                &response,
                &mut state.message_cache,
            );
            if response.finish_reason.as_deref() == Some("length")
                && assistant_msg
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
            {
                let count = state.retry_counters.bump_truncated_tool();
                if count <= self.config.truncated_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Truncated tool arguments — retrying ({}/{})",
                            count, self.config.truncated_tool_call_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
            }
            state.retry_counters.reset_truncated_tool();

            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            let tool_calls: Vec<ToolCall> = tool_calls
                .unwrap_or_default()
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                // C2 refactor: protocol fallback single point. The
                // Codex-ack / autonomous-nudge / patch-last-assistant /
                // finalize decisions used to be open-coded here and
                // duplicated in `run_blocking`. Now both call sites
                // delegate to `decide_empty_response`.
                let outcome = crate::empty_response_handler::decide_empty_response(
                    crate::empty_response_handler::EmptyResponseInput {
                        assistant_msg: &assistant_msg,
                        api_mode: self.config.api_mode.clone(),
                        has_tool_schemas: !tool_schemas.is_empty(),
                        history_includes_tool,
                        task_hint: &task_hint,
                        codex_ack_continuations,
                        autonomous_mode: self.config.autonomous_mode,
                        autonomous_nudge_count,
                        total_turns,
                        max_turns: self.config.max_turns,
                        max_autonomous_nudges: default_max_autonomous_nudges(),
                        last_content_with_tools: state.message_cache.take_content_with_tools(),
                        assistant_has_visible_text:
                            Self::assistant_visible_text_after_think_blocks,
                    },
                );
                codex_ack_continuations = outcome.new_codex_ack_continuations;
                autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                if self.apply_fallback_outcome(
                    &mut ctx,
                    outcome.action,
                    true, // stream_mode
                    total_turns,
                    autonomous_nudge_count,
                ) {
                    continue;
                }
                // Phase 2.4: terminal-persistence shared with run()
                self.persist_session_terminal(&ctx, session_id, total_turns, review_memory_at_end);
                Self::emit_mute_transition(&on_chunk, &stream_mute, false);
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    true,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            codex_ack_continuations = 0;

            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let all_housekeeping = tool_calls.iter().all(|tc| {
                matches!(
                    tc.function.name.as_str(),
                    "memory" | "todo" | "skill_manage" | "session_search"
                )
            });
            let should_mute_post =
                all_housekeeping && Self::assistant_visible_text_after_think_blocks(&assistant_msg);
            Self::emit_mute_transition(&on_chunk, &stream_mute, should_mute_post);

            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                let count = state.retry_counters.bump_invalid_tool();
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        count, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if count >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        false,
                    last_finish_reason.clone(),
                    ));
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    push_tool_result(&mut ctx, &tool_calls, &tc.id, &content);
                }
                continue;
            }
            state.retry_counters.reset_invalid_tool();

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                let count = state.retry_counters.bump_invalid_json();
                if count < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            count, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                state.retry_counters.reset_invalid_json();
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    push_tool_result(&mut ctx, &tool_calls, &tc.id, &content);
                }
                continue;
            }
            state.retry_counters.reset_invalid_json();
            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }
            self.cap_delegates(&mut tool_calls);

            // Pre-tool hooks + callbacks. Same `tc.id → uuid` map
            // pattern as the primary dispatch path (line ~3460) so
            // callers can pair start/complete events. The two
            // dispatch paths produce different `results` types
            // (synchronous gather here vs. parallel `execute_tool_calls`
            // in the primary path) but both end up keyed by
            // `result.tool_call_id == tc.id`.
            let mut tool_call_ids: HashMap<String, String> = HashMap::new();
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let tool_call_id = uuid::Uuid::new_v4().to_string();
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
            }

            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            let mut results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // Post-tool hooks + callbacks. Look up the uuid we
            // stashed in the pre-loop keyed by the same `tc.id` so
            // callers can correlate start/complete events.
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    let tool_call_id = tool_call_ids
                        .get(&tc.id)
                        .cloned()
                        .unwrap_or_else(|| format!("legacy-{}", tc.function.name));
                    cb(&tool_call_id, &tc.function.name, &res.content);
                }
            }

            // File-change summary events (mirrors the first on_tool_complete block).
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                if res.is_error {
                    continue;
                }
                if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                    if let Some(ref cb) = self.callbacks.on_tool_complete {
                        let synth_id = format!("file_change::{}", change.path);
                        let synth_name = format!("file_change::{}", change.path);
                        let payload = serde_json::json!({
                            "change_type": change.change_type,
                            "file_path": change.path,
                            "file_summary": change.summary,
                        }).to_string();
                        cb(&synth_id, &synth_name, &payload);
                    }
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }

            for result in results {
                // Look up the original tool name (e.g. "terminal")
                // from the matching tool_calls entry so reloaded
                // sessions can rebuild ToolMessageMeta with the real
                // label. Without this the persisted `Message::name`
                // is None and the UI falls back to a generic "tool"
                // pill on reload.
                push_tool_result(&mut ctx, &tool_calls, &result.tool_call_id, &result.content);
            }
            self.post_tool_apply_pending_steer(&mut ctx);
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            stream_needs_break.store(true, Ordering::Release);
            on_chunk(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({"control": "stream_break"})),
                }),
                finish_reason: None,
                usage: None,
            });
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_tokens = ctx.total_tokens();
            let threshold = ctx.pressure_threshold_tokens();
            if threshold > 0 {
                let progress = total_tokens as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if state.context_pressure.should_emit_warning(progress, tier) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        format_token_count(total_tokens),
                        format_token_count(threshold)
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx).await;

            // Mirror of the writeback at the end of `run()`: the only
            // loop-local still bridged into `state` is `total_turns`.
            // TODO(2026-07-04): see the dual-source note in `run()`
            // for the planned single-source collapse once
            // `run_shared_loop` lands.
            // See the bridge comment at the top of `run()` for history.
            state.total_turns = total_turns;
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

}
