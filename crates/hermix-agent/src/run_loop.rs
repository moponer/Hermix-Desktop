use std::sync::atomic::Ordering;
use crate::budget;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use serde_json::{json, Value};
use tracing::{info, instrument};
use hermix_core::{AgentError,AgentResult,format_token_count,LlmResponse,Message,MessageRole,StreamChunk,ToolCall,ToolErrorRecord,ToolSchema,UsageStats};
use crate::agent_helpers::default_max_autonomous_nudges;
use crate::agent_loop::{
    AgentLoop, LoopPhase, LoopState, PostLlmAction, PostResponseAction, PreparedRun, TurnRuntimeRoute,
    extract_last_user_assistant, extract_file_change,
};
use crate::context::ContextManager;
use crate::cost_guard::merge_usage;
use crate::plugins::HookType;
use crate::stream_collect::StreamCollectOutcome;
use crate::tool_result_processor::push_tool_result;

/// O1.1: return type for `do_pre_turn_setup` — either the agent
/// should stop (graceful interrupt / max-turns) or continue with
/// routing information for the LLM call.
enum PreTurnAction {
    /// Agent should stop with this result.
    Stop(AgentResult),
    /// Continue to LLM call with route + model info.
    Continue {
        turn_runtime_route: Option<TurnRuntimeRoute>,
        active_model: String,
    },
}

// ---------------------------------------------------------------------------
//skill auto-activation helper
// ---------------------------------------------------------------------------

/// Build the `<active-skills>` block to inject into ctx this turn.
/// Returns `None` when no skills match or when `user_text` is empty.
/// The block lists up to 3 skill names with their bodies (each body
/// capped at 500 lines by the loader, plus a total `char_budget`
/// cap here so even many skills can't blow the context).
/// Failure modes — none. We never panic or error out: missing
/// `skills_dir`, empty matches, FS errors all degrade to `None`.
pub fn build_active_skills_block(
    skills_dir: &std::path::Path,
    user_text: &str,
    char_budget: usize,
) -> Option<String> {
    if user_text.is_empty() {
        return None;
    }
    let mut loader = crate::skill_loader::SkillLoader::with_dir(skills_dir);
    loader.scan();
    let injection = loader.build_injection(user_text)?;

    if injection.len() <= char_budget {
        return Some(injection);
    }

    // Soft-cap: truncate at budget, leave room for the footer.
    let footer = format!(
        "\n\n[…truncated at {} chars total; use skill_view to load more.]",
        char_budget
    );
    let cap = char_budget.saturating_sub(footer.len());
    let mut out = injection;
    // `cap` is a byte budget; truncating at a non-UTF-8 boundary panics
    // (`is_char_boundary` assert) whenever the injection contains CJK —
    // floor to the nearest valid boundary instead.
    out.truncate(out.floor_char_boundary(cap));
    out.push_str(&footer);
    Some(out)
}

/// Outcome of [`AgentLoop::do_tool_turn`] — tells the caller what
/// to do after the tool execution block completes.
/// extracted from the duplicated tool-execution
/// blocks in `run()` and `run_stream()` (~120 lines saved).
enum ToolTurnOutcome {
    /// Rollback threshold was hit; the caller should `continue`
    /// the loop to start a fresh turn with the restored checkpoint.
    Rollback,
    /// Convergence detection triggered a hard stop; the caller
    /// should return this `AgentResult`.
    Converge(Result<AgentResult, AgentError>),
    /// The turn completed normally; the caller should proceed
    /// to the next loop iteration.
    Continue,
}

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    async fn do_pre_turn_setup(
        &self,
        ctx: &mut ContextManager,
        state: &mut LoopState,
        session_id: &str,
        tool_errors: &[ToolErrorRecord],
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finish_reason: Option<&str>,
    ) -> Result<PreTurnAction, AgentError> {
        // (type-B diagnostic): INFO log every pre-turn setup
        // entry so a future hang between turns shows up in stdout.
        // Without this, the only signal between `PostToolExec` and the
        // `PreTurnSetup` phase is a `tracing::debug!` that the default
        // log level never emits — and the user sees an unexplained
        // "● ● ● 正在思考" forever. Pair this with the matching
        // "post-tool finished" INFO at the end of
        // `do_post_tool_iteration` and you can pinpoint the hang to
        // exactly one frame.
        tracing::info!(
            turn    = state.total_turns + 1,
            session = session_id,
            "do_pre_turn_setup: entering"
        );

        // --- Graceful interrupt ---
        if self.interrupt.take_interrupt_graceful().is_some() {
            return Ok(PreTurnAction::Stop(self.graceful_interrupt_result(
                ctx,
                state.total_turns,
                tool_errors,
                state.cost_guard.accumulated_usage.clone(),
                state.cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
            )));
        }

        // poll any background-spawned compression ---
        // Non-blocking probe: if a previous spawn finished while the
        // agent loop was processing the last turn, roll the result
        // into `ctx` *before* anything else reads `ctx` (memory
        // recall, skill injection, LLM call). This is the only point
        // in the loop where the in-flight handle is reliably polled
        // every turn.
        let _ = self.poll_in_flight_compression(ctx, false).await;

        // --- `/steer` pre-API drain ---
        self.pre_api_apply_pending_steer(ctx);

        // --- Max turns guard ---
        // max_turns == 0 means unlimited — no turn cap. The real
        // protections are: ErrorLoop hard stop (convergence detector),
        // Cost Guard (max_cost_usd), and the LLM's natural stopping
        // behavior (no tool calls → finalize). max_turns is a
        // last-resort safety net for runaway loops that other
        // mechanisms don't catch.
        if self.config.max_turns > 0 && state.total_turns >= self.config.max_turns {
            tracing::warn!(
                "Max turns ({}) exceeded, requesting final summary",
                self.config.max_turns
            );
            self.emit_status(
                "lifecycle",
                &format!(
                    "已达到最大轮次限制 ({})，正在生成总结…",
                    self.config.max_turns
                ),
            );
            let summary_msg = self.handle_max_iterations(ctx).await?;
            if let Some(msg) = summary_msg {
                ctx.add_message(msg);
            }
            return Ok(PreTurnAction::Stop(self.finalize_agent_result(
                ctx,
                state.total_turns,
                tool_errors,
                state.cost_guard.accumulated_usage.clone(),
                state.cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
                false,
                false,
                finish_reason.map(|s| s.to_string()),
            )));
        }

        // --- Turn counter ---
        state.total_turns += 1;
        tracing::debug!("Agent turn {}", state.total_turns);

        // --- OAuth refresh ---
        self.oauth_refresh
            .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
            .await;

        // --- Skill nudge counter ---
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

        // --- Checkpoint ---
        if self.config.checkpoint_interval_turns > 0
            && (state.total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
        {
            state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
        }

        // --- Memory turn-start notification ---
        self.memory_on_turn_start(state.total_turns, "");

        // Removed Memory review hint (every 20 turns).
        // The system prompt already contains memory management guidance.
        // Injecting "review and consolidate" hints pollutes the context
        // and distracts the LLM from the user's actual task.

        // --- Memory sync at flush interval ---
        if state.total_turns.is_multiple_of(self.config.memory_flush_interval) && state.total_turns > 0 {
            let msgs = ctx.get_messages();
            let (u, a) = extract_last_user_assistant(msgs);
            self.memory_sync(&u, &a, session_id);
        }

        // --- Background snapshot + frame cap ---
        self.maybe_freeze_snapshot(session_id, state.total_turns);
        self.maybe_enforce_frame_cap(state.total_turns);

        //(A): per-turn memory recall ---
        // Cheap heuristic: every Nth turn (default 5) OR when the
        // user turn's first token differs from the previous recall,
        // we re-pull the top-N salient facts and emit a
        // `memory_recall` status event. The drift check uses the
        // first meaningful token of the user text (no embedding).
        let user_text: String = ctx
            .get_messages()
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("")
            .to_string();
        if let Some(plan) = self.maybe_recall(state, &user_text) {
            // Track keys for next-turn drift detection.
            state.last_recall_keys =
                plan.items.iter().map(|i| i.canonical_key.clone()).collect();
            // Inject recall block into ctx so the LLM can actually
            // use it. Previously this was emit-only (user could see
            // "thought of N items" but the agent couldn't). Now the
            // rendered block lands in message history as a system
            // message, same pattern as `memory_prefetch` at session
            // start. Compression will fold old recall blocks when
            // the context grows past the threshold.
            let recall_block = crate::recall_scheduler::render_prompt_block(&plan);
            if !recall_block.is_empty() {
                ctx.add_message(Message::system(&recall_block));
            }
        }

        //: auto-activate matching skills ---
        // Cheap keyword + subsequence-fuzzy match the user's message
        // against the skill directory and inject up to 3 skills'
        // full bodies into ctx. The skill bodies are appended as a
        // system message so the LLM sees them alongside the recall
        // block. Compression folds them on context pressure.
        if !user_text.is_empty() {
            let skills_dir =
                hermix_config::paths::hermix_home().join("skills");
            if let Some(trimmed) = build_active_skills_block(
                &skills_dir,
                &user_text,
                self.config.skill_injection_char_budget,
            ) {
                ctx.add_message(Message::system(&trimmed));
            }
        }

        // --- Pre-LLM hook + route resolution ---
        let turn_runtime_route = state
            .route_state
            .forced_runtime_route
            .clone()
            .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
        let active_model = turn_runtime_route
            .as_ref()
            .map(|r| r.model.as_str())
            .unwrap_or(self.config.model.as_str())
            .to_string();
        let hook_ctx = serde_json::json!({"turn": state.total_turns, "model": active_model});
        let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
        self.inject_hook_context(&pre_results, ctx);

        Ok(PreTurnAction::Continue {
            turn_runtime_route,
            active_model,
        })
    }

    /// O1.1: shared prefetch block — was ~80 lines of duplication between
    /// run() and run_stream(). Identical logic: intent classification,
    /// safe tool dispatch, thinking mode selection, ACUI emission.
    async fn do_prefetch(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
        total_turns: u32,
    ) {
        let Some(ref prefetcher) = self.prefetcher else {
            return;
        };
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

        // L4: dual-layer thinking mode selection
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

    /// O1.1: shared post-tool iteration block — was ~200 lines of
    /// duplication between run() and run_stream(). Handles: post-tool
    /// hooks, file-change events, memory/delegation notifications,
    /// budget enforcement, budget pressure, push results, steer,
    /// execute_code deduction, background review, context pressure,
    /// auto-compression, state writeback.
    #[allow(clippy::too_many_arguments)]
    async fn do_post_tool_iteration(
        &self,
        ctx: &mut ContextManager,
        tool_calls: &[ToolCall],
        results: Vec<hermix_core::ToolResult>,
        state: &mut LoopState,
        _session_id: &str,
        _tool_errors: &mut Vec<ToolErrorRecord>,
        _session_started_hooks_fired: bool,
        _persist_user_idx: Option<usize>,
        _review_memory_at_end: bool,
        total_turns: &mut u32,
        tool_call_ids: &HashMap<String, String>,
        stream_needs_break: Option<&AtomicBool>,
        on_chunk: Option<&(dyn Fn(StreamChunk) + Send + Sync)>,
    ) {
        // Post-tool hooks + callbacks
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
                cb(
                    &tool_call_id,
                    &tc.function.name,
                    &res.content,
                    res.denial_kind.as_deref().unwrap_or(""),
                );
            }
        }

        // File-change summary events
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
                    cb(&synth_id, &synth_name, &payload, "");
                }
            }
        }

        self.notify_memory_writes(tool_calls, &results);
        self.notify_delegations(tool_calls, &results);

        // Enforce budget on tool results
        let mut results = results;
        budget::enforce_budget(&mut results, &self.config.budget);

        // Removed verification notes injection and budget
        // pressure injection into tool results. These were polluting the
        // context with system-level messages that interfered with the
        // LLM's natural reasoning. The tool results themselves already
        // contain execution status. Cost guard warnings go to the UI,
        // not into the LLM's context.

        // (type-B diagnostic): emit an INFO log *before* the
        // post-tool writeback loop consumes `results` via `into_iter()`
        // — the count + error count would otherwise be unavailable.
        // Pair this with the `do_pre_turn_setup: entering` INFO at the
        // top of the next phase: if you see this INFO but never see
        // the pre-turn-setup INFO, the hang is in this body
        // (writeback / hooks / cost guard / `auto_compress`). If you
        // see the pre-turn-setup INFO but no `llm_response_received`
        // for the next turn, the hang is in `do_pre_turn_setup` or
        // `collect_stream_llm_response`.
        tracing::info!(
            turn    = state.total_turns,
            results = results.len(),
            errors  = results.iter().filter(|r| r.is_error).count(),
            "do_post_tool_iteration: post-tool results accepted, entering writeback & auto-compress"
        );

        // (v2 — zero-context-overhead model, inspired by
        // Hermes Agent's delegate_task design): When the LLM calls
        // `read_image` in the main context, NO image base64 is ever
        // injected into the main context. All base64 is stripped from
        // tool results (see `strip_image_base64` in
        // `tool_result_processor.rs`). Instead, a system nudge is
        // injected telling the LLM to use `delegate_task` for image
        // analysis — the sub-agent's isolated context absorbs the
        // base64 cost and destroys it when done.
        // This is the "zero-context-overhead" model: the main context
        // NEVER contains image base64, period. Even a single image
        // costs 2,000–8,000 tokens that are paid on every subsequent
        // turn — that's always wasteful.
        let mut read_image_paths: Vec<String> = Vec::new();
        for result in &results {
            if result.is_error {
                continue;
            }
            let Some(tc) = tool_calls.iter().find(|t| t.id == result.tool_call_id) else {
                continue;
            };
            if tc.function.name != "read_image" {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(&result.content) else {
                continue;
            };
            let img_path = parsed
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            read_image_paths.push(img_path.to_string());
        }

        if !read_image_paths.is_empty() {
            let total = read_image_paths.len();
            tracing::info!(
                paths = ?read_image_paths,
                total = total,
                "read_image called in main context — injecting delegate_task nudge (zero-context-overhead model)"
            );
            // Universal nudge: ALWAYS use delegate_task for image analysis.
            let paths_str = read_image_paths.join(", ");
            ctx.add_message(Message::system(&format!(
                "[SYSTEM — 图片分析必须使用 delegate_task]\n\
                 你刚才在主上下文中调用了 read_image 读取了 {total} 张图片：{paths}\n\n\
                 ⚠️ 图片 base64 已从工具结果中剥离，你无法在主上下文中看到图片内容。\n\n\
                 【必须操作】使用 `delegate_task` 将图片分析任务派给子 agent：\n\
                 - 子 agent 拥有隔离的上下文，可以自由调用 read_image 查看和分析图片\n\
                 - 子 agent 完成后只回传纯文字结论，图片 base64 不会进入你的上下文\n\
                 - 如有多张图片，可分批派发给多个子 agent（建议每批 3-5 张）\n\n\
                 示例：\n\
                 delegate_task(task=\"读取并分析图片，返回图片中可见的所有关键信息（文字、数字、证件字段等）的 JSON 摘要\", \\\n\
                 context=\"图片路径：{paths}\")\n\n\
                 🔴 禁止在主上下文中再次调用 read_image。所有图片分析必须通过 delegate_task 进行。\
                 这条规则没有例外——即使是单张图片也必须走子 agent。",
                total = total,
                paths = paths_str,
            )));
        }

        for result in results {
            // Record failed tools for the terminal hint's ping-pong
            // prevention: if a structured alternative already failed,
            // the hint won't suggest it again.
            if result.is_error {
                if let Some(name) = tool_calls
                    .iter()
                    .find(|tc| tc.id == result.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
                {
                    crate::tool_result_processor::record_tool_failure(name);
                }
            }
            // Apply terminal failure hint: if the terminal command failed
            // due to shell-compatibility issues, append a structured
            // suggestion to use move_file / read_file / execute_code instead.
            let tool_name = tool_calls
                .iter()
                .find(|tc| tc.id == result.tool_call_id)
                .map(|tc| tc.function.name.as_str());
            let content = crate::tool_result_processor::maybe_append_terminal_failure_hint(
                &result.content,
                tool_name,
                result.is_error,
            );
            push_tool_result(ctx, tool_calls, &result.tool_call_id, &content);
            // (human-approval fix): if this tool call was approved by the
            // USER clicking 允许 on the approval card (not an allowlist /
            // session-bypass auto-allow), inject an explicit system note so
            // the LLM knows the tool ran because the *user* consciously
            // approved it. Without this, the model cannot tell a manual
            // approval from a silent rule-based allow, and may report the
            // action as if it auto-passed. We consume the id so the note
            // fires exactly once, on the turn the tool actually executes.
            if let Some(ids) = &self.callbacks.human_approved_ids {
                let mut guard = ids.lock().expect("human_approved_ids poisoned");
                if guard.remove(&result.tool_call_id) {
                    drop(guard);
                    let name = tool_name.unwrap_or("<未知工具>");
                    ctx.add_message(Message::system(format!(
                        "[人工审批确认] 你刚才请求的 `{name}` 工具调用，已由用户在审批卡片上点击「允许」人工审查通过。\
                         这不是自动放行或白名单放行，而是用户有意识的人工批准。请尊重这一人工决策，\
                         并在向用户汇报时如实说明：该操作是经用户手动批准后才执行的。",
                        name = name,
                    )));
                }
            }
            // if the result is a *denial* (user拒绝 /
            // 超时 / 连接中断), inject the structured "tell the user"
            // system note right after the tool_result so the LLM's
            // next turn obeys it instead of going quiet or silently
            // retrying.
            crate::tool_result_processor::inject_denial_note_if_needed(
                ctx,
                tool_calls,
                &result,
            );
            // (sandbox-permission fix): if the tool failed with
            // a PERMISSION_DENIED error, inject the "ask the user to grant
            // access" guidance right after the denial note (the two are
            // orthogonal — a denial is a user refusal, a permission error is a
            // runtime restriction).
            crate::tool_result_processor::inject_permission_note_if_needed(
                ctx,
                tool_calls,
                &result,
            );
        }
        self.post_tool_apply_pending_steer(ctx);
        // Removed execute_code turn-counter deduction hack.
        // It was compensating for execute_code not counting as a "real"
        // turn, but this conflated turn accounting with tool semantics.
        // Trust the turn counter — every loop iteration is a turn.

        // Stream break control (stream-only)
        if let (Some(snb), Some(on_c)) = (stream_needs_break, on_chunk) {
            snb.store(true, Ordering::Release);
            on_c(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({"control": "stream_break"})),
                }),
                finish_reason: None,
                usage: None,
            });
        }

        self.emit_background_review_metrics(state.total_turns, ctx);

        // Context pressure check
        let total_tokens = ctx.total_tokens();
        let threshold = ctx.pressure_threshold_tokens();
        let max_tokens = ctx.max_context_tokens();
        // Fire a context_usage event every turn so the frontend can
        // render a persistent context-usage bar. We send it through
        // the on_chunk callback (same path as status/compaction events).
        if let Some(on_c) = on_chunk {
            let percent = if max_tokens > 0 {
                ((total_tokens as f64 / max_tokens as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            // Include cumulative cache hit/miss data from
            // the CostGuard so the frontend can display "缓存命中 45k"
            // alongside the context usage bar.
            let (cache_read, cache_creation) = state
                .cost_guard
                .accumulated_usage
                .as_ref()
                .map(|u| (u.cache_read_tokens, u.cache_creation_tokens))
                .unwrap_or((0, 0));
            on_c(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({
                        "event": "context_usage",
                        "used_tokens": total_tokens as u64,
                        "max_tokens": max_tokens as u64,
                        "threshold_tokens": threshold as u64,
                        "percent": percent,
                        "cache_read_tokens": cache_read,
                        "cache_creation_tokens": cache_creation,
                    })),
                }),
                finish_reason: None,
                usage: None,
            });
        }
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

        self.auto_compress_if_over_threshold(ctx).await;

        // After compression, emit an updated context_usage event so the
        // frontend progress bar reflects the reduced token count immediately
        // (the pre-compression event was already sent above with the old value).
        let new_total = ctx.total_tokens();
        if new_total < total_tokens {
            if let Some(on_c) = on_chunk {
                let new_percent = if max_tokens > 0 {
                    ((new_total as f64 / max_tokens as f64) * 100.0).min(100.0)
                } else {
                    0.0
                };
                on_c(StreamChunk {
                    delta: Some(hermix_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(serde_json::json!({
                            "event": "context_usage",
                            "used_tokens": new_total as u64,
                            "max_tokens": max_tokens as u64,
                            "threshold_tokens": threshold as u64,
                            "percent": new_percent,
                            "cache_read_tokens": 0,
                            "cache_creation_tokens": 0,
                        })),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
        // `total_turns` lives on `state` now (the previous `&mut u32`
        // parameter was hoisted into `LoopState`); no copy-back needed.
    }

    //: shared tool-execution + post-tool + convergence +
    // TurnReport block. Both `run()` and `run_stream()` had identical
    // ~60-line copies of this code. The only real differences are:
    // - log level (info vs debug for phase notification)
    // - `finalize_agent_result` params (is_stream flag + last_finish_reason)
    // - `do_post_tool_iteration` stream params (None vs Some)
    // All three are parameterized here.
    #[allow(clippy::too_many_arguments)]
    async fn do_tool_turn(
        &self,
        ctx: &mut ContextManager,
        state: &mut LoopState,
        tool_calls: &[ToolCall],
        total_turns: u32,
        tool_errors: &mut Vec<ToolErrorRecord>,
        session_id: &str,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        review_memory_at_end: bool,
        tool_call_ids: &HashMap<String, String>,
        response: &LlmResponse,
        is_stream: bool,
        last_finish_reason: Option<String>,
        stream_needs_break: Option<&Arc<AtomicBool>>,
        on_chunk: Option<&(dyn Fn(StreamChunk) + Send + Sync)>,
    ) -> ToolTurnOutcome {
        // --- tool execution ---
        let tool_start = Instant::now();
        if is_stream {
            tracing::debug!(
                phase = %LoopPhase::ToolExecution,
                turn = total_turns,
                tools = tool_calls.len(),
                "do_tool_turn: executing tool calls"
            );
        } else {
            tracing::info!(
                phase = %LoopPhase::ToolExecution,
                turn = total_turns,
                tools = tool_calls.len(),
                "do_tool_turn: executing tool calls"
            );
        }
        self.notify_phase(&LoopPhase::ToolExecution, total_turns);

        let results = self
            .execute_tool_calls(
                tool_calls,
                total_turns,
                self.config
                    .max_cost_usd
                    .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                tool_errors,
            )
            .await;
        state
            .timings
            .record_tool(tool_start.elapsed().as_millis() as u64);

        // --- Rollback check ---
        let turn_error_count = results.iter().filter(|r| r.is_error).count() as u32;
        if self.config.rollback_on_tool_error_threshold > 0
            && turn_error_count >= self.config.rollback_on_tool_error_threshold
        {
            if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                *ctx.get_messages_mut() = snapshot;
                ctx.add_message(Message::system(format!(
                    "Auto-rollback: {} tool call(s) failed in one turn. \
                     Restored latest checkpoint and continuing.",
                    turn_error_count
                )));
                ctx.recompute_total_tokens();
                return ToolTurnOutcome::Rollback;
            }
        }

        // --- post-tool execution ---
        tracing::debug!(
            phase = %LoopPhase::PostToolExec,
            turn = total_turns,
            "do_tool_turn: post-tool iteration"
        );
        self.notify_phase(&LoopPhase::PostToolExec, total_turns);

        let mut total_turns_mut = total_turns;
        self.do_post_tool_iteration(
            ctx,
            tool_calls,
            results.clone(),
            state,
            session_id,
            tool_errors,
            session_started_hooks_fired,
            persist_user_idx,
            review_memory_at_end,
            &mut total_turns_mut,
            tool_call_ids,
            stream_needs_break.map(|a| a.as_ref()),
            on_chunk,
        )
        .await;

        // --- Convergence detection ---
        let visible_text_this_turn = response.message.content.as_deref().unwrap_or("");
        let has_visible = Self::assistant_visible_text_after_think_blocks(&response.message);
        // Build per-tool failure/success lists for the convergence detector.
        // This lets it detect "terminal keeps failing even though read_file
        // succeeds" — a pattern the old aggregate error count missed.
        let failed_tool_names: Vec<&str> = results
            .iter()
            .filter(|r| r.is_error)
            .filter_map(|r| {
                tool_calls
                    .iter()
                    .find(|tc| tc.id == r.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
            })
            .collect();
        let succeeded_tool_names: Vec<&str> = results
            .iter()
            .filter(|r| !r.is_error)
            .filter_map(|r| {
                tool_calls
                    .iter()
                    .find(|tc| tc.id == r.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
            })
            .collect();
        let convergence_signal = state.convergence_detector.check(
            has_visible,
            tool_calls.len(),
            turn_error_count as usize,
            visible_text_this_turn,
            &failed_tool_names,
            &succeeded_tool_names,
        );
        match convergence_signal {
            crate::convergence_detector::ConvergenceSignal::Converge {
                reason,
                consecutive_count,
            } => {
                tracing::info!(
                    reason = ?reason,
                    consecutive = consecutive_count,
                    "convergence detected (hard stop) — concluding run"
                );
                self.persist_session_terminal(
                    ctx,
                    session_id,
                    total_turns,
                    review_memory_at_end,
                );
                let result = self.finalize_agent_result(
                    ctx,
                    total_turns,
                    tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    is_stream,
                    false,
                    last_finish_reason,
                );
                return ToolTurnOutcome::Converge(Ok(result));
            }
            _ => {}
        }

        // --- TurnReport ---
        crate::turn_report::TurnReport {
            turn: total_turns,
            end_phase: LoopPhase::PostToolExec,
            stage: LoopPhase::PostToolExec.as_orchestration_stage(),
            api_ms: state.timings.total_api_time_ms,
            tool_ms: state.timings.total_tool_time_ms,
            tool_calls: vec![],
            replay_count: 0,
            has_visible_text: has_visible,
            has_thinking: response
                .message
                .reasoning_content
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            prompt_tokens: response
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: response
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0),
            context_tokens: ctx.total_tokens() as u64,
            outcome: crate::turn_report::TurnOutcome::Continue,
            mode: None,
            task_id: None,
        }
        .emit();

        ToolTurnOutcome::Continue
    }

    #[instrument(
        name = "run",
        level = "info",
        skip_all,
        fields(
            model = %self.config.model,
            provider = %self.config.provider.as_deref().unwrap_or(""),
            msg_count = messages.len(),
            // Recorded after prepare_run_context resolves the actual
            // tool set (from registry when caller passes None).
            // Using Empty here avoids the misleading tool_count=0
            // when tools=None (the normal path from local_agent_service).
            tool_count = tracing::field::Empty,
            total_turns = tracing::field::Empty,
            total_ms = tracing::field::Empty,
        )
    )]
    //
    //read path 不得套 streaming state machine
    //早期设计试图在 read path（DB → ctx）中嵌入 streaming
    //   状态机，导致 read 延迟不可预测 + 状态不一致。
    //   修复规则：read path 只做纯数据加载，streaming 逻辑只在
    //   write path（LLM → ctx → DB）中运行。
    //   测试：run_loop_tests（隐式 — run 函数的 read/write 分离覆盖）
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        // Reset per-run terminal hint counter so a new user message
        // gets a fresh allowance of failure hints.
        crate::tool_result_processor::reset_terminal_hint_counter();

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
        // Record the RESOLVED tool count (after routing) so the span
        // shows how many tools the LLM actually sees, not the raw
        // parameter count (which is 0 when caller passes None).
        tracing::Span::current().record("tool_count", tool_schemas.len());
tracing::debug!(phase = %LoopPhase::Init, "Agent loop initialized (run)");
self.notify_phase(&LoopPhase::Init, 0);
let mut empty_stop_retry_count: u32 = 0;
        let mut post_tool_summarize_count: u32 = 0;

        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();

        //introduced `state` as a bridge — every
        // loop-local mutable would eventually live in `state` so a future
        // `run_shared_loop(state, llm_caller)` helper could take only
        // `&mut LoopState` instead of the original 11+ locals.
        //(..) collapsed the bridge to its
        // minimum: 4 manager structs (`cost_guard` / `retry_counters` /
        // `context_pressure` / `timings` / `message_cache`) own the
        // in-place-mutated fields, and `total_turns` is the only scalar
        // still aliased to a local. The other 13 locals were either
        // dropped or replaced with `record_*` / `bump_*` / `store_*`
        // methods on the inner structs.
        let mut state = LoopState::default();
        // LoopState owns total_turns; aliased here for hot-path readability.
        let mut total_turns: u32 = state.total_turns;

        // armed by `do_post_tool_iteration` once a turn
        // finishes tool execution; checked at the top of the next
        // iteration of this loop. Mirrors the run_stream() watchdog.
        let mut transition_started_at: Option<Instant> = None;

        loop {
            // turn transition watchdog (issue: tool approved
            // → next turn never starts). Records the timestamp right
            // after `do_post_tool_iteration` returns. If the next
            // iteration of this loop has not crossed the next
            // checkpoint within `turn_transition_deadline_secs` (the
            // pre-turn setup tracing line below), the loop is
            // considered stalled and we emit a terminal Done so the
            // WS client recovers — instead of the user being stuck on
            // the "正在思考…" indicator.
            if let (Some(start), false) = (
                transition_started_at,
                self.config.turn_transition_deadline_secs == 0,
            ) {
                let elapsed = start.elapsed();
                let deadline = std::time::Duration::from_secs(self.config.turn_transition_deadline_secs);
                if elapsed > deadline {
                    tracing::warn!(
                        target: "hermix.agent",
                        turn = total_turns,
                        elapsed_secs = elapsed.as_secs(),
                        deadline_secs = self.config.turn_transition_deadline_secs,
                        "turn_transition_stall (run): no progress since post-tool-iteration; \
                         emitting Done to recover WS client"
                    );
                    self.persist_session_terminal(
                        &ctx,
                        session_id,
                        total_turns,
                        review_memory_at_end,
                    );
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
                        Some("turn_transition_stall".to_string()),
                    ));
                }
            }
            // Reset the tracker: this iteration crossed the pre-turn
            // checkpoint, so any prior stall has cleared.
            transition_started_at = None;

            tracing::debug!(phase = %LoopPhase::PreTurnSetup, turn = total_turns, "run: pre-turn setup");
self.notify_phase(&LoopPhase::PreTurnSetup, total_turns);
            // O1.1: shared pre-turn setup (was ~100 lines of duplication
            // between run() and run_stream()). Extracted to do_pre_turn_setup.
            let (turn_runtime_route, _active_model) = match self.do_pre_turn_setup(
                &mut ctx,
                &mut state,
                session_id,
                &tool_errors,
                session_started_hooks_fired,
                persist_user_idx,
                None, // finish_reason — run() doesn't track this
            ).await? {
                PreTurnAction::Continue { turn_runtime_route, active_model } =>
                    (turn_runtime_route, active_model),
                PreTurnAction::Stop(result) => return Ok(result),
            };
            // Sync: do_pre_turn_setup increments state.total_turns;
            // the local alias must be refreshed so downstream code
            // (logging, process_post_response, decide_empty_response,
            // finalize_agent_result) sees the correct turn number.
            total_turns = state.total_turns;

            // --- Prefetch: identify user intent, run safe tools, inject results as system prompt ---
            tracing::debug!(phase = %LoopPhase::Prefetch, turn = total_turns, "run: prefetch");
self.notify_phase(&LoopPhase::Prefetch, total_turns);
            self.do_prefetch(&mut ctx, &tool_schemas, total_turns).await;

            // --- LLM API call with transport retry + semantic empty/thinking recovery (Python parity) ---
            tracing::debug!(phase = %LoopPhase::LlmCall, turn = total_turns, "run: LLM API call");
self.notify_phase(&LoopPhase::LlmCall, total_turns);
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
                    // Non-interrupt errors (API error, network timeout,
                    // credential failure) — convert to graceful interrupt
                    // so the ctx messages are returned and persisted.
                    // Without this, returning Err(e) causes
                    // local_agent_service's `?` to skip the post-run
                    // persist, and the session reloads empty.
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "LLM call failed (non-stream path) — treating as graceful interrupt for persistence"
                        );
                        return Ok(self.graceful_interrupt_result_errored(
                            &ctx,
                            total_turns,
                            &tool_errors,
                            state.cost_guard.accumulated_usage.clone(),
                            state.cost_guard.session_cost_usd,
                            session_started_hooks_fired,
                            persist_user_idx,
                            Some(e.to_string()),
                        ));
                    }
                };
                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // push the per-turn usage snapshot so
                // the front-end cache-hit chip updates in real time.
                // Emitted *after* merge so the chip reflects the
                // running total across the current turn, not just
                // the latest chunk. Quiet-mode short-circuits inside
                // `emit_usage_update`.
                if let Some(ref u) = turn_usage_acc {
                    self.emit_usage_update(u);
                }

                //: ~40 lines of has_tools /
                // visible_text / reasoning-retry / empty-retry /
                // exhausted classification now lives in
                //`classify_post_llm_response` . The
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
            //: record timings on `state.timings`
            // directly (saturating-add inside) instead of accumulating
            // to a local and bridging back via the now-removed
            // `publish_from` helper.
            state.timings.record_api(api_elapsed);

            //.3b : post-LLM hook + usage accumulate +
            // cost guard extracted into `process_post_response`. ~60 lines
            // of duplication with `run_stream()` collapsed into one helper
            // call.
            tracing::debug!(phase = %LoopPhase::PostLlmCall, turn = total_turns, "run: post-LLM response processing");
self.notify_phase(&LoopPhase::PostLlmCall, total_turns);
            //follow-up: both `CostGuardTripped` and `Finalize`
            // must short-circuit here. The previous version only matched
            // `CostGuardTripped` and silently fell through to the
            // `decide_empty_response` block below, which in autonomous_mode
            // with history_includes_tool=true issues an autonomous nudge
            // ("Continue working...") and causes the LLM to re-issue the
            // same tool (e.g. two `dir /b` rows).
            match self.process_post_response(
                &mut ctx,
                &response,
                turn_usage_acc.as_ref(),
                total_turns,
                api_elapsed,
                &mut state.cost_guard,
                &mut state.route_state,
                &mut state.message_cache,
                false, // stream_mode
            )? {
                PostResponseAction::CostGuardTripped => {
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
                PostResponseAction::Finalize => {
                    // Normal flow: LLM produced visible text → finalize.
                    // The thinking went to reasoning_content (collapsible
                    // block in UI), the visible text is rendered for the user.
                    if Self::assistant_visible_text_after_think_blocks(
                        &response.message
                    ) {
                        // In autonomous_mode, if the LLM produced
                        // visible text but no tool calls and no tools have
                        // been used yet, fall through to decide_empty_response
                        // instead of finalizing. The autonomous nudge will push
                        // the LLM to start using tools. Without this, the agent
                        // would stop after the LLM says "I'll help you with that"
                        // without actually doing any work.
                        let history_has_tool = ctx
                            .get_messages()
                            .iter()
                            .any(|m| m.role == MessageRole::Tool);
                        let should_autonomous_nudge = self.config.autonomous_mode
                            && !history_has_tool
                            && autonomous_nudge_count == 0
                            && (self.config.max_turns == 0
                                || total_turns < self.config.max_turns)
                            && !tool_schemas.is_empty();
                        if !should_autonomous_nudge {
                            // WW: persist the final assistant
                            // message into ctx BEFORE terminal persist. The
                            // Continue branch falls through to
                            // `prepare_assistant_turn` below, but the
                            // visible-text Finalize branch used to skip it —
                            // causing `persist_session_terminal(&ctx, …)` and
                            // `finalize_agent_result(&ctx, …)` to see a ctx
                            // missing the final assistant message, so DB /
                            // disk `.md` only stored up to the tool result
                            // and the reload view came back truncated.
                            let _ = self.prepare_assistant_turn(
                                &mut ctx,
                                &response,
                                &mut state.message_cache,
                            );
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
                        // Fall through to prepare_assistant_turn +
                        // decide_empty_response — the autonomous nudge
                        // will fire and push the LLM to use tools.
                    }
                    // No visible text (thinking-only or empty) — fall
                    // through to decide_empty_response for edge-case
                    // handling (Codex ack / autonomous nudge / patch).
                    // No multi-round nudges — trust the LLM + system prompt.
                }
                PostResponseAction::Continue => {
                    // Tool calls present — fall through to execute_tool_calls.
                }
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
            // filter out empty-named tool calls (same fix as
            // run_stream). MiniMax streaming can create placeholder entries.
            let tool_calls_filtered: Vec<ToolCall> = tool_calls
                .unwrap_or_default()
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();
            let tool_calls = if tool_calls_filtered.is_empty() {
                // If all tool calls had empty names but the LLM produced
                // visible text, finalize — it gave a real answer.
                // If only thinking (no visible text), fall through to
                // decide_empty_response for edge-case handling.
                let has_visible =
                    Self::assistant_visible_text_after_think_blocks(&assistant_msg);
                if has_visible {
                    tracing::info!(
                        "All tool calls had empty names after filtering (run); LLM produced visible text — finalizing"
                    );
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
// No visible text — fall through to decide_empty_response
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
                        empty_stop_retry_count,
                        post_tool_summarize_count,
                        total_turns,
                        max_turns: self.config.max_turns,
                        max_autonomous_nudges: default_max_autonomous_nudges(),
                        assistant_has_visible_text:
                            Self::assistant_visible_text_after_think_blocks,
                    },
                );
                codex_ack_continuations = outcome.new_codex_ack_continuations;
                autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                empty_stop_retry_count = outcome.new_empty_stop_retry_count;
                post_tool_summarize_count = outcome.new_post_tool_summarize_count;
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
                    //terminal-persistence shared with run_stream
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
            } else {
                tool_calls_filtered
            };

            codex_ack_continuations = 0;

            // Deduplicate tool calls
            tracing::debug!(phase = %LoopPhase::ToolValidation, turn = total_turns, "run: tool validation");
self.notify_phase(&LoopPhase::ToolValidation, total_turns);
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }

            //AL-7 : Unified tool-call validation.
            let available = self.tool_registry.names().join(", ");
            let outcome = crate::tool_call_validator::validate(
                &mut tool_calls,
                |name| self.tool_registry.get(name).is_some(),
                |tc| Self::normalize_tool_call_arguments(tc),
                &mut state.retry_counters,
                &crate::tool_call_validator::ValidatorConfig {
                    max_invalid_tool_retries: self.config.invalid_tool_call_max_retries,
                    max_invalid_json_retries: self.config.invalid_tool_json_max_retries,
                    available_tools: &available,
                },
            );
            match &outcome {
                crate::tool_call_validator::ValidationOutcome::RetryInvalidTool { .. } => {
                    self.emit_status("lifecycle", "Invalid tool call detected — retrying");
                }
                crate::tool_call_validator::ValidationOutcome::FatalInvalidTool { invalid_names } => {
                    self.emit_status("lifecycle", &format!(
                        "Max invalid tool retries reached ({})", self.config.invalid_tool_call_max_retries
                    ));
                    let _ = invalid_names; // already in system message via apply_outcome
                }
                crate::tool_call_validator::ValidationOutcome::RetryInvalidJson => {
                    self.emit_status("lifecycle", "Invalid tool JSON arguments — retrying");
                }
                crate::tool_call_validator::ValidationOutcome::FatalInvalidJson { .. } => {
                    self.emit_status("lifecycle", &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ));
                }
                crate::tool_call_validator::ValidationOutcome::Valid => {}
            }
            let apply_result = crate::tool_call_validator::apply_outcome(
                &mut ctx, &tool_calls, outcome,
            );
            match apply_result {
                crate::tool_call_validator::ApplyResult::Proceed => {}
                crate::tool_call_validator::ApplyResult::Continue => continue,
                crate::tool_call_validator::ApplyResult::ReturnFatal => {
                    // #A (global fix): an invalid-tool exhaustion is a
                    // recoverable backend failure, NOT a clean completion.
                    // `interrupted=true` so the server sets the `Done`
                    // outcome to "interrupted" and the front-end keeps the
                    // session alive (no WS teardown, pending approvals
                    // preserved) instead of silently killing the turn.
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        true,
                        None,
                    ));
                }
            }

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

            // Removed cross-turn tool-call deduplication.
            // It was caching tool results and skipping re-execution, which
            // prevented legitimate re-checks (e.g. re-reading a file after
            // modifying it). The LLM has full context and can decide for
            // itself whether to re-call a tool. Trust the LLM.

            // --- Pre-tool hooks + callbacks ---
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
                    let tool_call_id = format!("{}-{}", tc.function.name, tc.id);
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
                // O7.2: fire clarify callback when the agent asks the user a question
                if tc.function.name == "clarify" {
                    if let Some(ref cb) = self.callbacks.on_clarify {
                        let question = tc.function.arguments.clone();
                        cb(&question);
                    }
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
            let outcome = self.do_tool_turn(
                &mut ctx,
                &mut state,
                &tool_calls,
                total_turns,
                &mut tool_errors,
                session_id,
                session_started_hooks_fired,
                persist_user_idx,
                review_memory_at_end,
                &tool_call_ids,
                &response,
                false, // is_stream
                None,  // last_finish_reason
                None,  // stream_needs_break
                None,  // on_chunk
            ).await;
            match outcome {
                ToolTurnOutcome::Rollback => continue,
                ToolTurnOutcome::Converge(result) => return result,
                ToolTurnOutcome::Continue => {}
            }
            // arm the turn-transition watchdog. Mirrors
            // the run_stream() side. The next loop iteration's
            // top-of-loop check will fire if we don't reach the
            // pre-turn-setup checkpoint within
            // `turn_transition_deadline_secs` of now.
            transition_started_at = Some(Instant::now());
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
            // Recorded after prepare_run_context resolves the actual
            // tool set (from registry when caller passes None).
            tool_count = tracing::field::Empty,
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
        // Reset per-run terminal hint counter so a new user message
        // gets a fresh allowance of failure hints.
        crate::tool_result_processor::reset_terminal_hint_counter();

        let _run_started = Instant::now();
        info!(
            msg_count = messages.len(),
            tools_passed = tools.as_ref().map(|t| t.len()).unwrap_or(0),
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
        //: `run_stream` setup was an open-coded
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
        // Record the RESOLVED tool count (after routing) so the span
        // shows how many tools the LLM actually sees.
        tracing::Span::current().record("tool_count", tool_schemas.len());
        tracing::debug!(phase = %LoopPhase::Init, "Agent loop initialized (run_stream)");
self.notify_phase(&LoopPhase::Init, 0);
        let mut empty_stop_retry_count: u32 = 0;
        let mut post_tool_summarize_count: u32 = 0;
        let session_id = self.config.session_id.as_deref().unwrap_or("");

        let mut state = LoopState::default();
        //: same as in `run`. Only `total_turns`
        // keeps the local-alias pattern. The two message caches live
        // on `state.message_cache` (mutated in place) and the two
        // timings live on `state.timings` (mutated in place).
        let mut total_turns: u32 = state.total_turns;

        // armed by `do_post_tool_iteration` once a turn
        // finishes tool execution; checked at the top of the next
        // iteration of this loop. See watchdog block below.
        let mut transition_started_at: Option<Instant> = None;

        loop {
            // turn transition watchdog (issue: tool approved
            // → next turn never starts). Records the timestamp right
            // after `do_post_tool_iteration` returns. If the next
            // iteration of this loop has not crossed the next
            // checkpoint within `turn_transition_deadline_secs` (the
            // pre-turn setup tracing line below), the loop is
            // considered stalled and we emit a terminal Done so the
            // WS client recovers — instead of the user being stuck on
            // the "正在思考…" indicator.
            if let (Some(start), false) = (
                transition_started_at,
                self.config.turn_transition_deadline_secs == 0,
            ) {
                let elapsed = start.elapsed();
                let deadline = std::time::Duration::from_secs(self.config.turn_transition_deadline_secs);
                if elapsed > deadline {
                    tracing::warn!(
                        target: "hermix.agent",
                        turn = total_turns,
                        elapsed_secs = elapsed.as_secs(),
                        deadline_secs = self.config.turn_transition_deadline_secs,
                        "turn_transition_stall: no progress since post-tool-iteration; \
                         emitting Done to recover WS client"
                    );
                    self.persist_session_terminal(
                        &ctx,
                        session_id,
                        total_turns,
                        review_memory_at_end,
                    );
                    let mut fr = last_finish_reason.clone();
                    if fr.is_none() {
                        fr = Some("turn_transition_stall".to_string());
                    }
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
                        fr,
                    ));
                }
            }
            // Reset the tracker: this iteration crossed the pre-turn
            // checkpoint, so any prior stall has cleared. The next
            // post-tool-iteration will set it again.
            transition_started_at = None;

            tracing::debug!(phase = %LoopPhase::PreTurnSetup, turn = total_turns, "run_stream: pre-turn setup");
self.notify_phase(&LoopPhase::PreTurnSetup, total_turns);
            // O1.1: shared pre-turn setup (was ~90 lines of duplication
            // between run() and run_stream()). Extracted to do_pre_turn_setup.
            let (turn_runtime_route, active_model) = match self.do_pre_turn_setup(
                &mut ctx,
                &mut state,
                session_id,
                &tool_errors,
                session_started_hooks_fired,
                persist_user_idx,
                last_finish_reason.as_deref(),
            ).await? {
                PreTurnAction::Continue { turn_runtime_route, active_model } =>
                    (turn_runtime_route, active_model),
                PreTurnAction::Stop(result) => return Ok(result),
            };
            // Sync: do_pre_turn_setup increments state.total_turns;
            // the local alias must be refreshed so downstream code
            // (logging, process_post_response, decide_empty_response,
            // finalize_agent_result) sees the correct turn number.
            total_turns = state.total_turns;

            // --- Streaming first attempt + same D-step semantic recovery as `run()` (retries use non-stream) ---
            tracing::debug!(phase = %LoopPhase::LlmCall, turn = total_turns, "run_stream: LLM API call (streaming)");
self.notify_phase(&LoopPhase::LlmCall, total_turns);
            // Emit a status event so the frontend knows the agent is
            // still alive between turns. Without this, after a long
            // delegate_task returns and the loop starts a new LLM call,
            // the UI shows the "stop" button with zero activity — no
            // thinking dot, no status chip — for potentially minutes
            // until the first streaming chunk arrives. This status event
            // resets the idle/stale timers on the frontend and shows
            // "正在思考…（第 N 轮）" so the user knows the agent is working.
            self.emit_status("thinking", &format!("正在思考…（第 {} 轮）", total_turns));
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
                            &active_model,
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
                        Ok(StreamCollectOutcome::Interrupted(_, _)) => serde_json::json!({
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
                    // If collect_stream_llm_response itself returns Err
                    // (e.g. provider build failure, credential error),
                    // convert to graceful interrupt so the ctx messages
                    // are returned and persisted by the caller. Without
                    // this, the `?` would propagate the error,
                    // local_agent_service's `?` would skip the post-run
                    // persist, and the session would reload empty.
                    let stream_outcome = match stream_collect {
                        Ok(outcome) => outcome,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "collect_stream_llm_response failed — treating as graceful interrupt for persistence"
                            );
                            return Ok(self.graceful_interrupt_result_errored(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                Some(e.to_string()),
                            ));
                        }
                    };
                    match stream_outcome {
                        StreamCollectOutcome::Complete(resp) => resp,
                        StreamCollectOutcome::Interrupted(partial, captured_error) => {
                            state
                                .cost_guard
                                .record_usage(partial.usage.as_ref(), partial.model.as_str(), &self.config);
                            // A mid-stream chunk failure (silence_watch /
                            // network blip) carries `finish_reason =
                            // "stream_error"` on the partial response; a
                            // user stop carries "interrupted". Only the
                            // former is a backend failure that must be
                            // surfaced to the frontend as an error.
                            // `captured_error` is the raw transport/provider
                            // error text (e.g. an LLM 400), threaded into
                            // `AgentResult::last_error` so the frontend can
                            // show the real cause instead of a generic banner.
                            let errored = partial.finish_reason.as_deref() == Some("stream_error");
                            // A stream that died mid-tool-call persists a half
                            // assistant whose tool_calls never ran, so no tool
                            // result will ever pair with them. Replaying that
                            // history trips strict providers (MiniMax-M3) with
                            // 400 "(2013) tool call result does not follow tool
                            // call". Strip the unexecuted calls here — the
                            // prose is kept, the tools simply get re-invoked on
                            // the next turn.
                            let mut partial_message = partial.message;
                            if partial_message.tool_calls.is_some() {
                                let stripped = partial_message
                                    .tool_calls
                                    .as_ref()
                                    .map(|tcs| tcs.len())
                                    .unwrap_or(0);
                                partial_message.tool_calls = None;
                                tracing::warn!(
                                    stripped = stripped,
                                    "interrupted stream: discarding unexecuted tool_calls \
                                     before persisting partial assistant message"
                                );
                            }
                            ctx.add_message(partial_message);
                            return Ok(self.graceful_interrupt_result_with(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                errored,
                                captured_error,
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
                        // Non-interrupt errors (API error, network timeout,
                        // credential failure) — convert to graceful interrupt
                        // so the ctx messages are returned and persisted.
                        // Without this, returning Err(e) causes
                        // local_agent_service's `?` to skip the post-run
                        // persist, and the session reloads empty.
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "LLM call failed (stream retry path) — treating as graceful interrupt for persistence"
                            );
                            return Ok(self.graceful_interrupt_result_errored(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                Some(e.to_string()),
                            ));
                        }
                    }
                };
                inner_attempt = inner_attempt.saturating_add(1);

                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // per-turn usage push for the cache-hit
                // chip. Mirrors the run() side above; emitted on
                // every streaming chunk so the chip updates
                // continuously while the response streams in.
                if let Some(ref u) = turn_usage_acc {
                    self.emit_usage_update(u);
                }

                //: mirrors the run side —
                // ~40 lines of has_tools / visible_text /
                // reasoning-retry / empty-retry / exhausted
                // classification collapse into a single `match` on
                //`classify_post_llm_response` . The
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
            // O1.1 fix: run_stream() was missing the API timing record
            // that run() has at line 327. Without it, dashboard cost
            // reports under-count stream-path API time.
            state.timings.record_api(api_elapsed_ms);

            //.3c : mirrors 1.3b on the run_stream
            // side. Post-LLM hook + usage accumulate + cost guard move
            // into `process_post_response`. The stream-specific
            tracing::debug!(phase = %LoopPhase::PostLlmCall, turn = total_turns, "run_stream: post-LLM response processing");
self.notify_phase(&LoopPhase::PostLlmCall, total_turns);
            // `llm_response_received` info! line is now emitted inside
            // the helper when `stream_mode = true`. The
            // `last_finish_reason` was already updated inside the
            // stream retry loop (line ~5640) to the final response's
            // finish_reason — no need to refresh it here.
            //follow-up : `process_post_response` returns
            // `Finalize` when there are no tool calls in the response — its
            // doc-comment (agent_loop/types.rs:156-165) explicitly says
            // "the caller should `return` Ok(self.finalize_agent_result(...))".
            // The previous version of this site only matched on
            // `CostGuardTripped`, silently falling through into the empty-
            // tool-calls block at line ~1247 below. That block then runs
            // `decide_empty_response`, which (in autonomous_mode with
            // history_includes_tool=true and counter==0) issues a
            // `ContinueWithSystemNudge("...continue working...")` and the
            // loop calls the LLM again. For MiniMax-M2.7 the second LLM
            // call often re-issues the same tool the first turn used
            // (e.g. `dir /b` twice, two assistant bubbles, two summaries)
            // because the nudge tells the model "you have not yet
            // completed the user's task". Observed live in the
            // 沪EB5805/沪EB5885 \"看下目录\" session.
            // Fix: honor the `Finalize` return — drop straight into the
            // terminal finalize path with `from_post_response=true` so
            // the persistence hook still runs.
            match self.process_post_response(
                &mut ctx,
                &response,
                turn_usage_acc.as_ref(),
                total_turns,
                api_elapsed_ms,
                &mut state.cost_guard,
                &mut state.route_state,
                &mut state.message_cache,
                true, // stream_mode
            )? {
                PostResponseAction::CostGuardTripped => {
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
                PostResponseAction::Finalize => {
                    // Normal flow: LLM produced visible text → finalize.
                    // The thinking went to reasoning_content (collapsible
                    // block in UI), the visible text is rendered for the user.
                    if Self::assistant_visible_text_after_think_blocks(
                        &response.message
                    ) {
                        // In autonomous_mode, if the LLM produced
                        // visible text but no tool calls and no tools have
                        // been used yet, fall through to decide_empty_response
                        // instead of finalizing. The autonomous nudge will push
                        // the LLM to start using tools. Without this, the agent
                        // would stop after the LLM says "I'll help you with that"
                        // without actually doing any work.
                        let history_has_tool = ctx
                            .get_messages()
                            .iter()
                            .any(|m| m.role == MessageRole::Tool);
                        let should_autonomous_nudge = self.config.autonomous_mode
                            && !history_has_tool
                            && autonomous_nudge_count == 0
                            && (self.config.max_turns == 0
                                || total_turns < self.config.max_turns)
                            && !tool_schemas.is_empty();
                        if !should_autonomous_nudge {
                            // WW: persist the final assistant
                            // message into ctx BEFORE terminal persist. The
                            // Continue branch falls through to
                            // `prepare_assistant_turn` below, but the
                            // visible-text Finalize branch used to skip it —
                            // causing `persist_session_terminal(&ctx, …)` and
                            // `finalize_agent_result(&ctx, …)` to see a ctx
                            // missing the final assistant message, so DB /
                            // disk `.md` only stored up to the tool result
                            // and the reload view came back truncated. Owner
                            // saw this live on the 沪EB5805 "看下目录" session:
                            // streaming showed the full "用户，目录共 198 条..."
                            // text but reload was truncated to assistant1+tool.
                            let _ = self.prepare_assistant_turn(
                                &mut ctx,
                                &response,
                                &mut state.message_cache,
                            );
                            self.persist_session_terminal(
                                &ctx,
                                session_id,
                                total_turns,
                                review_memory_at_end,
                            );
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
                        // Fall through to prepare_assistant_turn +
                        // decide_empty_response — the autonomous nudge
                        // will fire and push the LLM to use tools.
                    }
                    // No visible text (thinking-only or empty) — fall
                    // through to decide_empty_response for edge-case
                    // handling (Codex ack / autonomous nudge / patch).
                    // No multi-round nudges — trust the LLM + system prompt.
                }
                PostResponseAction::Continue => {
                    // Tool calls present — fall through to the
                    // execute_tool_calls path below.
                }
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
                    ctx.recompute_total_tokens();
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
                // If all tool calls had empty names after filtering but
                // the LLM produced visible text, respect the stop signal
                // and finalize. If only thinking, fall through to
                // decide_empty_response for edge-case handling.
                let has_visible =
                    Self::assistant_visible_text_after_think_blocks(&assistant_msg);
                if has_visible {
                    tracing::info!(
                        "All tool calls had empty names after filtering; LLM produced visible text — finalizing"
                    );
                    self.persist_session_terminal(
                        &ctx,
                        session_id,
                        total_turns,
                        review_memory_at_end,
                    );
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
                //( streaming hang fix): when the LLM
                // returned tool_calls but all were filtered out AND the
                // turn history already contains tool results, the
                // decide_empty_response path can loop forever —
                // autonomous_mode fires a ContinueWithSystemNudge that
                // tells the LLM "you haven't finished", the LLM again
                // returns empty tool_calls, and the loop repeats until
                // max_turns. Fix: if the current turn produced tool
                // calls that were filtered (history_includes_tool=true)
                // and there's no visible text, finalize immediately
                // instead of looping. The model chose to stop —
                // respect its stop signal.
                if history_includes_tool {
                    tracing::info!(
                        turn = total_turns,
                        "All tool calls were filtered out and history already includes tools; finalizing immediately (no empty-response loop)"
                    );
                    self.persist_session_terminal(
                        &ctx,
                        session_id,
                        total_turns,
                        review_memory_at_end,
                    );
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
                        empty_stop_retry_count,
                        post_tool_summarize_count,
                        total_turns,
                        max_turns: self.config.max_turns,
                        max_autonomous_nudges: default_max_autonomous_nudges(),
                        assistant_has_visible_text:
                            Self::assistant_visible_text_after_think_blocks,
                    },
                );
                codex_ack_continuations = outcome.new_codex_ack_continuations;
                autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                empty_stop_retry_count = outcome.new_empty_stop_retry_count;
                post_tool_summarize_count = outcome.new_post_tool_summarize_count;
                if self.apply_fallback_outcome(
                    &mut ctx,
                    outcome.action,
                    true, // stream_mode
                    total_turns,
                    autonomous_nudge_count,
                ) {
                    continue;
                }
                //terminal-persistence shared with run
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

            tracing::debug!(phase = %LoopPhase::ToolValidation, turn = total_turns, "run_stream: tool validation");
self.notify_phase(&LoopPhase::ToolValidation, total_turns);
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

            //AL-7 : Unified tool-call validation.
            let available = self.tool_registry.names().join(", ");
            let outcome = crate::tool_call_validator::validate(
                &mut tool_calls,
                |name| self.tool_registry.get(name).is_some(),
                |tc| Self::normalize_tool_call_arguments(tc),
                &mut state.retry_counters,
                &crate::tool_call_validator::ValidatorConfig {
                    max_invalid_tool_retries: self.config.invalid_tool_call_max_retries,
                    max_invalid_json_retries: self.config.invalid_tool_json_max_retries,
                    available_tools: &available,
                },
            );
            match &outcome {
                crate::tool_call_validator::ValidationOutcome::RetryInvalidTool { .. } => {
                    self.emit_status("lifecycle", "Invalid tool call detected — retrying");
                }
                crate::tool_call_validator::ValidationOutcome::FatalInvalidTool { invalid_names } => {
                    self.emit_status("lifecycle", &format!(
                        "Max invalid tool retries reached ({})", self.config.invalid_tool_call_max_retries
                    ));
                    let _ = invalid_names;
                }
                crate::tool_call_validator::ValidationOutcome::RetryInvalidJson => {
                    self.emit_status("lifecycle", "Invalid tool JSON arguments — retrying");
                }
                crate::tool_call_validator::ValidationOutcome::FatalInvalidJson { .. } => {
                    self.emit_status("lifecycle", &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ));
                }
                crate::tool_call_validator::ValidationOutcome::Valid => {}
            }
            let apply_result = crate::tool_call_validator::apply_outcome(
                &mut ctx, &tool_calls, outcome,
            );
            match apply_result {
                crate::tool_call_validator::ApplyResult::Proceed => {}
                crate::tool_call_validator::ApplyResult::Continue => continue,
                crate::tool_call_validator::ApplyResult::ReturnFatal => {
                    // #A (global fix): invalid-tool exhaustion is a
                    // recoverable backend failure → mark `interrupted` so
                    // the front-end keeps the session alive (see run_loop
                    // ~1418 for the matching change in the other call site).
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        true,
                        last_finish_reason.clone(),
                    ));
                }
            }
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

            // Removed cross-turn tool-call deduplication.
            // The LLM has full context and can decide for itself whether
            // to re-call a tool. Caching prevented legitimate re-checks.

            // Pre-tool hooks + callbacks
            let mut tool_call_ids: HashMap<String, String> = HashMap::new();
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let tool_call_id = format!("{}-{}", tc.function.name, tc.id);
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
                if tc.function.name == "clarify" {
                    if let Some(ref cb) = self.callbacks.on_clarify {
                        let question = tc.function.arguments.clone();
                        cb(&question);
                    }
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

            let outcome = self.do_tool_turn(
                &mut ctx,
                &mut state,
                &tool_calls,
                total_turns,
                &mut tool_errors,
                session_id,
                session_started_hooks_fired,
                persist_user_idx,
                review_memory_at_end,
                &tool_call_ids,
                &response,
                true,  // is_stream
                last_finish_reason.clone(),
                Some(&stream_needs_break),
                Some(on_chunk.as_ref()),
            ).await;
            match outcome {
                ToolTurnOutcome::Rollback => continue,
                ToolTurnOutcome::Converge(result) => return result,
                ToolTurnOutcome::Continue => {}
            }
            // arm the turn-transition watchdog. The next
            // loop iteration's top-of-loop check will fire if we
            // don't reach the pre-turn-setup checkpoint within
            // `turn_transition_deadline_secs` of now.
            transition_started_at = Some(Instant::now());
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Verify tool execution results and inject verification status.
    /// This is the "verify work" step from Claude Code's gather→act→verify pattern.
    /// Checks file modifications, terminal exit codes, and tool errors.
    pub fn verify_tool_results(
        &self,
        tool_calls: &[ToolCall],
        results: &[hermix_core::ToolResult],
    ) -> Vec<String> {
        let mut verification_notes = Vec::new();

        for res in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                continue;
            };

            if res.is_error {
                // Tool execution failed
                verification_notes.push(format!(
                    "VERIFICATION: {} failed - {}",
                    tc.function.name, res.content
                ));
                continue;
            }

            // Check file modification results
            if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                verification_notes.push(format!(
                    "VERIFICATION: {} modified {} ({})",
                    tc.function.name, change.path, change.change_type
                ));
            }

            // Check terminal command results
            if tc.function.name == "terminal" {
                if let Ok(data) = serde_json::from_str::<Value>(&res.content) {
                    if let Some(exit_code) = data.get("exit_code").and_then(|v| v.as_i64()) {
                        if exit_code != 0 {
                            verification_notes.push(format!(
                                "VERIFICATION: {} exited with code {}",
                                tc.function.name, exit_code
                            ));
                        }
                    }
                }
            }
        }

        verification_notes
    }

    /// Inject verification notes into the context as a system message.
    pub fn inject_verification_notes(
        ctx: &mut ContextManager,
        notes: &[String],
    ) {
        if notes.is_empty() {
            return;
        }
        let verification_text = format!(
            "[VERIFICATION]\n{}\n\nIf any verification failed, investigate and fix before proceeding.",
            notes.join("\n")
        );
        ctx.add_message(Message::system(&verification_text));
    }

}

// ---------------------------------------------------------------------------
//tests for `build_active_skills_block`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod p2_t1_tests {
    use super::build_active_skills_block;
    use std::fs;

    fn seed_three_skills(dir: &std::path::Path) {
        let skills = [
            (
                "context-compression",
                "Best practices for lossy compression of conversation history.",
                "compression",
            ),
            (
                "memory-systems",
                "Architecture patterns for persistent agent memory across sessions.",
                "memory",
            ),
            (
                "tool-design",
                "Principles for designing LLM-callable tools that are safe and effective.",
                "tools",
            ),
        ];
        for (slug, desc, cat) in skills {
            let d = dir.join(slug);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("SKILL.md"),
                format!(
                    "---\nname: {slug}\ndescription: {desc}\ncategory: {cat}\n\
                     tags: [{cat}]\n---\n# {slug}\n\nDo the {slug} thing properly.\n"
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn p2_t1_block_injects_matching_skills_into_injection() {
        let tmp = tempfile::tempdir().unwrap();
        seed_three_skills(tmp.path());

        // "memory" exact-matches memory-systems; "compression" exact-
        // matches context-compression (both whole-word hits in their
        // descriptions). tool-design should not appear (no token
        // overlap from the query).
        let block = build_active_skills_block(
            tmp.path(),
            "show me memory and compression patterns",
            16_000,
        )
        .expect("expected at least one match");

        assert!(block.contains("memory-systems"));
        assert!(block.contains("context-compression"));
        assert!(!block.contains("tool-design"));
    }

    #[test]
    fn p2_t1_fuzzy_subseq_within_word_activates_skill() {
        // Typo-style within-word subseq: "compres" hits
        // context-compression's description. The skill must
        // surface even though `compres` is not a whole-word in
        // any skill description.
        let tmp = tempfile::tempdir().unwrap();
        seed_three_skills(tmp.path());
        let block = build_active_skills_block(
            tmp.path(),
            "compres",
            16_000,
        )
        .expect("expected fuzzy match");

        assert!(block.contains("context-compression"));
    }

    #[test]
    fn p2_t1_block_returns_none_for_irrelevant_query() {
        let tmp = tempfile::tempdir().unwrap();
        seed_three_skills(tmp.path());

        let block = build_active_skills_block(
            tmp.path(),
            "asdfqwer zxcv ytrewq",
            16_000,
        );
        assert!(block.is_none());
    }

    #[test]
    fn p2_t1_block_returns_none_for_empty_user_text() {
        let tmp = tempfile::tempdir().unwrap();
        seed_three_skills(tmp.path());
        assert!(build_active_skills_block(tmp.path(), "", 16_000).is_none());
    }

    #[test]
    fn p2_t1_block_truncates_when_budget_exceeded() {
        // Pad each skill body past the char budget so the
        // joined block would exceed it; helper must truncate
        // with the footer and stay under (budget + footer).
        let tmp = tempfile::tempdir().unwrap();
        let padding = "x".repeat(200);
        for slug in ["long-a", "long-b", "long-c"] {
            let d = tmp.path().join(slug);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("SKILL.md"),
                format!(
                    "---\nname: {slug}\ndescription: long skill {slug}\ntags: [long]\n---\n{padding}\n{padding}\n{padding}\n"
                ),
            )
            .unwrap();
        }

        let block = build_active_skills_block(
            tmp.path(),
            "show me a long skill please",
            2_000,
        )
        .expect("match expected");
        assert!(
            block.contains("truncated at 2000 chars total"),
            "footer missing — block: …{}",
            &block[block.len().saturating_sub(120)..]
        );
        // Body section may exceed budget by footer length only —
        // i.e. total ≤ budget + footer length.
        assert!(block.len() <= 2_000 + 80);
    }

    #[test]
    fn p2_t1_block_truncates_cjk_safely_at_budget() {
        // Regression: `out.truncate(cap)` panicked with
        // `assertion failed: self.is_char_boundary(new_len)` whenever the
        // budget fell mid-way through a multi-byte CJK character — every
        // Chinese-language skill (custom-app-development etc.) that
        // exceeded the budget crashed the whole turn. The truncated
        // string must be a valid UTF-8 boundary.
        let tmp = tempfile::tempdir().unwrap();
        // English descriptions so the ASCII query matches (the matcher
        // splits on whitespace), but the *bodies* are CJK — that's the
        // shape of real Chinese skills, and where the byte-level truncate
        // used to explode.
        let pad_line = "自适应应用开发注意事项核心概念实践指南模板示例集成参考。".repeat(4);
        for slug in ["long-a", "long-b", "long-c"] {
            let d = tmp.path().join(slug);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("SKILL.md"),
                format!(
                    "---\nname: {slug}\ndescription: long skill {slug}\ntags: [long]\n---\n{pad_line}\n{pad_line}\n"
                ),
            )
            .unwrap();
        }

        let block = build_active_skills_block(tmp.path(), "show me a long skill please", 2_000)
            .expect("match expected");
        assert!(
            block.contains("truncated at 2000 chars total"),
            "footer missing"
        );
        // The whole block must be valid UTF-8 (would panic if we'd kept
        // the byte-level truncate) and stay within budget + footer.
        assert!(std::str::from_utf8(block.as_bytes()).is_ok());
        assert!(block.len() <= 2_000 + 80);
    }
}
