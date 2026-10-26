use std::sync::atomic::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use serde_json::Value;
use tracing::{info, instrument};
use hermix_core::{AgentError,AgentResult,Message,MessageRole,StreamChunk,ToolCall,ToolSchema,UsageStats};
use crate::agent_helpers::default_max_autonomous_nudges;
use crate::agent_loop::{
    AgentLoop, LoopPhase, LoopState, PostLlmAction, PostResponseAction, PreparedRun, TurnRuntimeRoute,
};
use crate::cost_guard::merge_usage;
use crate::plugins::HookType;
use crate::stream_collect::StreamCollectOutcome;

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
    build_active_skills_block_with_tools(skills_dir, user_text, char_budget)
        .map(|(block, _)| block)
}

/// Same as [`build_active_skills_block`] but also returns the union of the
/// matched skills' `allowed-tools` (None = unrestricted). The loop stores it
/// on `LoopState` for the turn and subtracts it from the dispatch surface —
/// skill activation can only narrow, never widen, the session toolset.
pub fn build_active_skills_block_with_tools(
    skills_dir: &std::path::Path,
    user_text: &str,
    char_budget: usize,
) -> Option<(String, Option<std::collections::HashSet<String>>)> {
    if user_text.is_empty() {
        return None;
    }
    let mut loader = crate::skill_loader::SkillLoader::with_dir(skills_dir);
    loader.scan();
    let (injection, tools) = loader.build_injection_and_tools(user_text)?;

    if injection.len() <= char_budget {
        return Some((injection, tools));
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
    Some((out, tools))
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

// --- split-out impl blocks (pure relocation, see module docs) ---
mod pre_turn;
mod tool_prep;
mod tool_turn;

use tool_prep::ToolPrep;
mod verification;

impl AgentLoop {
    /// Arm the one-shot stop-signal forwarder (2026-10-18).
    ///
    /// The server's per-turn `shutdown` Notify is the wire-level Stop
    /// request (`WsClientMessage::Stop` → `notify_one`). Until now that
    /// Notify was only consumed by the approval-wait select, so a Stop
    /// during an in-flight LLM call did nothing — the turn kept running
    /// (minutes, on slow providers) and a re-sent message was deferred
    /// behind it. This task converts the FIRST notify into a real
    /// `InterruptController::interrupt()`, activating every cancellation
    /// consumer: the llm_retry O1.2 `select!` (in-flight LLM call dies
    /// immediately), the approval-wait select, and all `check_interrupt`
    /// polls. The loop then drains to `interrupted` and the terminal
    /// `Done(outcome="interrupted")` frame is the frontend's cleanup
    /// trigger.
    ///
    /// One-shot by design: a stopped turn drains to its terminal frame;
    /// notifies arriving after this turn are stored permits on a Notify
    /// instance that dies with this agent task — harmless.
    fn arm_stop_signal_forwarder(&self) {
        if let Some(shutdown) = self.callbacks.shutdown.clone() {
            let interrupt = self.interrupt.clone();
            tokio::spawn(async move {
                shutdown.notified().await;
                tracing::info!(
                    "stop signal forwarded: InterruptController::interrupt()"
                );
                interrupt.interrupt(None);
            });
        }
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
        self.arm_stop_signal_forwarder();

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
        let mut state = LoopState::with_convergence(
            crate::convergence_detector::ConvergenceConfig::for_domain(
                self.config.domain_override,
            ),
        );
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
                &mut state.acceptance_gate,
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
                            crate::session_lifecycle::flush_acceptance_advisory(
                                &mut ctx,
                                &mut state.acceptance_gate,
                            );
                            return Ok(self.finalize_agent_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                // 可见文本自然收尾 —— 与 run_stream() 的同一分支
                                // (line ~2126) 及"工具名全被过滤后仍收尾"分支保持
                                // 一致。此前这里传 false，导致非流式路径（CLI /
                                // 部分 API 路由）把每一次正常回复都上报成
                                // `finished_naturally=false`（success=false）。
                                true,
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
            crate::session_lifecycle::flush_acceptance_advisory(
                &mut ctx,
                &mut state.acceptance_gate,
            );

            // Truncated-args gate (same as run_stream): streaming can end
            // mid-tool-call (finish_reason="length" from
            // has_truncated_tool_args); executing the half JSON would
            // feed a tool error into the convergence detector. Pop the
            // assistant message and retry within the cap.
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
                // 自治模式豁免：可见文本 + 还没动过工具 + nudge 额度在时，
                // 不在此 finalize —— 落到 decide_empty_response 让 nudge 推
                // LLM 开始干活。上方 Finalize 分支的豁免落到这里，而这段
                // MiniMax 空名过滤的 finalize 曾把它短路（nudge 测试
                // call_count==1 的回归根因）。
                let nudge_eligible = self.config.autonomous_mode
                    && !tool_schemas.is_empty()
                    && !history_includes_tool
                    && autonomous_nudge_count == 0
                    && (self.config.max_turns == 0
                        || total_turns < self.config.max_turns);
                if has_visible && !nudge_eligible {
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

            // tool-call 前置处理（去重 → 修补 → 校验 → 计数器 → delegate 上限
            // → PreToolCall 钩子 → 中断检查）与 `run_stream` 共用同一实现，
            // 见 `run_loop/tool_prep.rs`。此前两份逐字重复且已分叉。
            self.notify_phase(&LoopPhase::ToolValidation, total_turns);
            let (tool_calls, tool_call_ids) = match self.prepare_tool_calls(
                &mut ctx,
                &mut state,
                tool_calls,
                total_turns,
                &tool_errors,
                session_started_hooks_fired,
                persist_user_idx,
                None,
                "run",
                None,
            )? {
                ToolPrep::Proceed {
                    tool_calls,
                    tool_call_ids,
                } => (tool_calls, tool_call_ids),
                ToolPrep::SkipTurn => continue,
                ToolPrep::Abort(result) => return Ok(*result),
            };

            // --- Execute tool calls in parallel ---
            // arm the turn-transition watchdog HERE — before tool
            // execution — so the top-of-loop check on the next iteration
            // covers the whole real stall window: tool execution incl.
            // the approval wait (the #1 unbounded-await risk) and the
            // pre-turn setup. Arming AFTER do_tool_turn (the old spot)
            // left zero awaits between arm and check, making the
            // watchdog structurally unable to fire.
            transition_started_at = Some(Instant::now());
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
        self.arm_stop_signal_forwarder();

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

        let mut state = LoopState::with_convergence(
            crate::convergence_detector::ConvergenceConfig::for_domain(
                self.config.domain_override,
            ),
        );
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
                &mut state.acceptance_gate,
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
                            crate::session_lifecycle::flush_acceptance_advisory(
                                &mut ctx,
                                &mut state.acceptance_gate,
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
            crate::session_lifecycle::flush_acceptance_advisory(
                &mut ctx,
                &mut state.acceptance_gate,
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
                // 自治模式豁免（与 run() 同一意图、同一根因）：nudge 额度在
                // 且还没动过工具时不 finalize，落到 decide_empty_response。
                let nudge_eligible = self.config.autonomous_mode
                    && !tool_schemas.is_empty()
                    && !history_includes_tool
                    && autonomous_nudge_count == 0
                    && (self.config.max_turns == 0
                        || total_turns < self.config.max_turns);
                if has_visible && !nudge_eligible {
                    tracing::info!(
                        "All tool calls had empty names after filtering; LLM produced visible text — finalizing"
                    );
                    // 失败必须看得见：模型想调的工具因 name 缺失被丢弃
                    // （MiniMax-M3 流式 delta 缺陷），本轮"说了要干活但
                    // 没干"就静默收场会让用户以为卡死。emit 一条可见
                    // 状态，前端据此在消息尾部提示，而不是无声结束。
                    self.emit_status(
                        "lifecycle",
                        "工具调用未能解析（模型未返回工具名），本轮已按普通回复结束；可重发继续",
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
            // 流式独有：家务型工具批次 + 已有可见文本时进入静音过渡。它必须发生在
            // **去重之后、校验之前**（历史顺序），所以作为钩子传给共用实现。
            let mute_hook = |calls: &[ToolCall]| {
                let all_housekeeping = calls.iter().all(|tc| {
                    matches!(
                        tc.function.name.as_str(),
                        "memory" | "todo" | "skill_manage" | "session_search"
                    )
                });
                let should_mute_post = all_housekeeping
                    && Self::assistant_visible_text_after_think_blocks(&assistant_msg);
                Self::emit_mute_transition(&on_chunk, &stream_mute, should_mute_post);
            };
            let (tool_calls, tool_call_ids) = match self.prepare_tool_calls(
                &mut ctx,
                &mut state,
                tool_calls,
                total_turns,
                &tool_errors,
                session_started_hooks_fired,
                persist_user_idx,
                last_finish_reason.clone(),
                "run_stream",
                Some(&mute_hook),
            )? {
                ToolPrep::Proceed {
                    tool_calls,
                    tool_call_ids,
                } => (tool_calls, tool_call_ids),
                ToolPrep::SkipTurn => continue,
                ToolPrep::Abort(result) => return Ok(*result),
            };

            // arm the turn-transition watchdog HERE — before tool
            // execution — same rationale as the run() side: the old
            // arm-after-execution spot had no awaits between arm and
            // check, so the watchdog could never fire. Arming before
            // do_tool_turn covers the approval wait + tool execution.
            transition_started_at = Some(Instant::now());
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
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------


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
