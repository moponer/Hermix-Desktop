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
use crate::context::ContextManager;
use crate::cost_guard::{merge_usage, CostGuard};
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
    // 复用进程级缓存：`with_dir + scan()` 实测 25.6ms/次且同步阻塞在 async
    // 路径上。失效规则见 `skill_loader::cached_loader` 的文档（正文改动仍然
    // 立即生效，只有 frontmatter 元数据最多延迟 60s）。
    let loader = crate::skill_loader::cached_loader(skills_dir);
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

/// 流式调用上下文（`run_stream` 专属）。字段均 `Copy`，`call_turn_llm` 在
/// 重试循环里反复 `match` 它而不移动借用。
#[derive(Clone, Copy)]
struct StreamCallCtx<'a> {
    active_model: &'a str,
    on_chunk: &'a (dyn Fn(StreamChunk) + Send + Sync),
}

/// `call_turn_llm` 的终止信号：调用方据此经 `finish_terminated_turn`
/// 转成对应的 graceful 结果，避免在 `call_turn_llm` 内部与 `&mut LoopState`
/// 抢借用。
enum TurnLlmTermination {
    /// 用户中断（stop 按钮 / WS 断开 / `call_llm_with_retry` 的 Interrupted）。
    UserInterrupted,
    /// 后端错误（非中断），携带错误文本。
    Errored(String),
    /// 流式中途断流（`StreamCollectOutcome::Interrupted`）。partial 已由
    /// `call_turn_llm` 消费（strip 未执行的 tool_calls + `ctx.add_message` +
    /// `record_usage`）；`errored` 区分 `stream_error` 与用户中断。
    StreamInterrupted { errored: bool, captured_error: Option<String> },
}

/// `call_turn_llm` 的结果：要么拿到可继续处理的响应（附本轮累积 usage，
/// 供调用方传给 `process_post_response`），要么需要终止。
enum TurnLlmOutcome {
    Response { response: hermix_core::LlmResponse, turn_usage_acc: Option<UsageStats> },
    Terminate(TurnLlmTermination),
}

/// Turn transition watchdog：工具轮结束（`do_post_tool_iteration` 返回）后
/// [`Self::arm`]，下一轮 pre-turn 检查 [`Self::stalled`]；跨过检查点即
/// [`Self::disarm`]。`turn_transition_deadline_secs == 0` 时关闭（legacy 行为）。
/// 抽取自 `run()` / `run_stream()` 各一份的 48 行 watchdog 块。
struct TurnBudget {
    deadline_secs: u64,
    armed_at: Option<Instant>,
}

impl TurnBudget {
    fn new(deadline_secs: u64) -> Self {
        Self { deadline_secs, armed_at: None }
    }

    /// 工具轮结束，开始计时到下一轮 pre-turn 检查点。
    fn arm(&mut self) {
        self.armed_at = Some(Instant::now());
    }

    /// 布防且超时 → 返回 `(elapsed_secs, deadline_secs)` 供日志使用。
    fn stalled(&self) -> Option<(u64, u64)> {
        let start = self.armed_at?;
        if self.deadline_secs == 0 {
            return None;
        }
        let elapsed = start.elapsed();
        (elapsed > std::time::Duration::from_secs(self.deadline_secs))
            .then(|| (elapsed.as_secs(), self.deadline_secs))
    }

    /// 本次迭代跨过了 pre-turn 检查点，先前的 stall 已清除。
    fn disarm(&mut self) {
        self.armed_at = None;
    }
}

/// `run()` / `run_stream()` 共享的回合准备产物：`prepare_run_context` 的解构 +
/// `LoopState` + 计数器 + watchdog，原先两条路径各写一遍（~25 行 × 2）。
/// 调用方解构回原有的局部变量名，后续代码零改动。
struct RunSetup {
    ctx: ContextManager,
    tool_schemas: Vec<ToolSchema>,
    session_started_hooks_fired: bool,
    persist_user_idx: Option<usize>,
    codex_ack_continuations: u32,
    autonomous_nudge_count: u32,
    review_memory_at_end: bool,
    task_hint: String,
    state: LoopState,
    empty_stop_retry_count: u32,
    post_tool_summarize_count: u32,
    turn_budget: TurnBudget,
}

// --- split-out impl blocks (pure relocation, see module docs) ---
mod pre_turn;
mod tool_prep;
mod tool_turn;

use tool_prep::ToolPrep;
mod verification;

/// 「本可收尾、但任务未必完成」时最多自动续跑多少次。
///
/// `max_turns` 默认 0（不限），所以自动续跑需要自己的硬上限：模型可能陷进
/// 「调一个工具 → 写一段总结 → 被推回去 → 再调一个工具」的磨蹭循环，靠
/// 收敛检测（连错 / 零新信息）不一定能收住。
const MAX_GOAL_CONTINUATIONS: u32 = 40;

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

    /// 「这一轮可以收尾了，但目标是不是真的完成了？」——判定要不要把模型
    /// 推回去继续干。
    ///
    /// 旧判据是 `!history_has_tool`（本次运行还没用过任何工具），语义只覆盖
    /// 「模型光说不做」这一种情形。长任务一旦动过工具，`history_has_tool`
    /// 永久为真，于是「干一段 → 写总结 → 交回用户」成为唯一出口，用户必须
    /// 每轮手动打「继续」。这里换成目标驱动：
    ///
    /// - `autonomous_nudge_count < default_max_autonomous_nudges()`：连续
    ///   「只说话不动手」的轮数还没用满预算 → 可以推。
    /// - 模型每次重新调用工具都会把该计数清零（见 `codex_ack_continuations = 0`
    ///   的同一位置），所以「干活 → 收尾总结 → 被推回 → 再干活」可以一直循环
    ///   到目标完成。
    /// - 连续推满 `default_max_autonomous_nudges`（默认 8）次仍然只说话 →
    ///   判定它确实没活可干，放行收尾（避免把它推进空转）。
    /// - `goal_continuations` 是全程硬上限，防止磨蹭型任务无限烧钱。
    ///
    /// 2026-10-30：判据曾是 `autonomous_nudge_count == 0`（只能推一次），而
    /// `max_autonomous_nudges`（默认 8）虽然一路传进 `decide_empty_response`
    /// 却从未被读过 —— 两个预算都是死的，长任务里"写一段总结就收尾"成了唯一
    /// 出口。现在两处统一走本函数返回的同一个 blocker，避免两份真相源互相
    /// 抵消（另一处曾是 `nudge_eligible`）。
    fn goal_continue_blocker(
        &self,
        autonomous_nudge_count: u32,
        goal_continuations: u32,
        total_turns: u32,
        tool_schemas_empty: bool,
    ) -> Option<&'static str> {
        if !self.config.autonomous_mode {
            return Some("autonomous_mode_off");
        }
        if tool_schemas_empty {
            return Some("no_tool_schemas");
        }
        if autonomous_nudge_count >= default_max_autonomous_nudges() {
            return Some("nudge_budget_exhausted");
        }
        if goal_continuations >= MAX_GOAL_CONTINUATIONS {
            return Some("goal_continuation_cap");
        }
        if self.config.max_turns != 0 && total_turns >= self.config.max_turns {
            return Some("turn_budget");
        }
        None
    }

    fn should_goal_continue(
        &self,
        autonomous_nudge_count: u32,
        goal_continuations: u32,
        total_turns: u32,
        tool_schemas_empty: bool,
    ) -> bool {
        self.goal_continue_blocker(
            autonomous_nudge_count,
            goal_continuations,
            total_turns,
            tool_schemas_empty,
        )
        .is_none()
    }

    /// `run()` / `run_stream()` 共享的回合准备：`prepare_run_context` → 解构 →
    /// `LoopState` / 计数器 / watchdog 初始化。原先两条路径各写一遍（~25 行），
    /// 现在只剩这一份；`path` 只用于 init 日志文本，保留路径区分。
    async fn prepare_run_state(
        &self,
        messages: Vec<Message>,
        tools: Option<&Vec<ToolSchema>>,
        path: &str,
    ) -> RunSetup {
        crate::tool_result_processor::reset_terminal_hint_counter();
        self.arm_stop_signal_forwarder();
        let PreparedRun {
            ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            codex_ack_continuations,
            autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        } = self.prepare_run_context(messages, tools).await;
        // Record the RESOLVED tool count (after routing) so the span
        // shows how many tools the LLM actually sees, not the raw
        // parameter count (which is 0 when caller passes None).
        tracing::Span::current().record("tool_count", tool_schemas.len());
        tracing::debug!(phase = %LoopPhase::Init, "Agent loop initialized ({path})");
        self.notify_phase(&LoopPhase::Init, 0);
        RunSetup {
            ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            codex_ack_continuations,
            autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
            state: LoopState::with_convergence(
                crate::convergence_detector::ConvergenceConfig::for_domain(
                    self.config.domain_override,
                ),
            ),
            empty_stop_retry_count: 0,
            post_tool_summarize_count: 0,
            turn_budget: TurnBudget::new(self.config.turn_transition_deadline_secs),
        }
    }

    /// watchdog 超时的终止路径：持久化 + finalize（`finished_naturally=true`、
    /// `interrupted=false`）。`finish_reason` 由调用方给——`run()` 传
    /// `turn_transition_stall`，`run_stream()` 优先用 `last_finish_reason`。
    fn finalize_transition_stall(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        state: &LoopState,
        session_id: &str,
        review_memory_at_end: bool,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finish_reason: Option<String>,
    ) -> AgentResult {
        self.persist_session_terminal(ctx, session_id, total_turns, review_memory_at_end);
        self.finalize_agent_result(
            ctx,
            total_turns,
            tool_errors,
            state.cost_guard.accumulated_usage.clone(),
            state.cost_guard.session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            true,
            false,
            finish_reason,
        )
    }

    /// 把 [`Self::call_turn_llm`] 返回的终止信号转成对应的 graceful `AgentResult`。
    /// 三种终止对应三种 graceful 构造，usage 与 cost 从 `cost_guard` 快照读取。
    fn finish_terminated_turn(
        &self,
        ctx: &ContextManager,
        t: TurnLlmTermination,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        cost_guard: &CostGuard,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
    ) -> AgentResult {
        match t {
            TurnLlmTermination::UserInterrupted => self.graceful_interrupt_result(
                ctx,
                total_turns,
                tool_errors,
                cost_guard.accumulated_usage.clone(),
                cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
            ),
            TurnLlmTermination::Errored(msg) => self.graceful_interrupt_result_errored(
                ctx,
                total_turns,
                tool_errors,
                cost_guard.accumulated_usage.clone(),
                cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
                Some(msg),
            ),
            TurnLlmTermination::StreamInterrupted { errored, captured_error } => {
                self.graceful_interrupt_result_with(
                    ctx,
                    total_turns,
                    tool_errors,
                    cost_guard.accumulated_usage.clone(),
                    cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    errored,
                    captured_error,
                )
            }
        }
    }

    /// 单轮 LLM 调用循环（流式优先、空响应降级非流式、empty/thinking 重试）。
    ///
    /// 把 `run()`（纯非流式）与 `run_stream()`（流式优先）里逐字重复的
    /// 「interrupt 检查 → 调用 → usage 累积 → classify」骨架收成单一实现，
    /// 重试策略只剩这一个实现点。`stream` 为 `None` 时走纯非流式（`run()`），
    /// `Some` 时流式优先、失败降级非流式（`run_stream()`）。
    ///
    /// 终止不在这里构造 `AgentResult`：返回 [`TurnLlmTermination`]，由调用方
    /// 经 [`Self::finish_terminated_turn`] 统一转 graceful 结果，避免与
    /// `&mut LoopState` 抢借用。流式 `Interrupted` 的 partial 已在此消费
    /// （strip 未执行的 tool_calls + `ctx.add_message` + `record_usage`）。
    /// 流式 `Complete` 的 `finish_reason` 经 `last_finish_reason`（`Some` 时）
    /// 无条件写回，与旧 `run_stream` 的即时更新语义一致。
    #[allow(clippy::too_many_arguments)]
    async fn call_turn_llm(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
        turn_runtime_route: Option<&TurnRuntimeRoute>,
        cost_guard: &mut CostGuard,
        stream: Option<StreamCallCtx<'_>>,
        mut last_finish_reason: Option<&mut Option<String>>,
    ) -> TurnLlmOutcome {
        let mut inner_empty = 0u32;
        let mut inner_thinking = 0u32;
        let mut turn_usage_acc: Option<UsageStats> = None;
        let mut inner_attempt: u32 = 0;
        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return TurnLlmOutcome::Terminate(TurnLlmTermination::UserInterrupted);
            }
            let r: hermix_core::LlmResponse = match stream {
                Some(sc) if inner_attempt == 0 => {
                    // 流式首轮（run_stream 独有）：PreApiRequest hook → 流式收集 → PostApiRequest hook。
                    let pre_api_ctx = serde_json::json!({
                        "attempt": inner_attempt,
                        "model": sc.active_model,
                        "stream": true,
                        "route_label": turn_runtime_route
                            .as_ref()
                            .and_then(|r| r.route_label.as_deref()),
                    });
                    let _ = self.invoke_hook(HookType::PreApiRequest, &pre_api_ctx);
                    let stream_collect = self
                        .collect_stream_llm_response(
                            ctx,
                            tool_schemas,
                            turn_runtime_route,
                            sc.active_model,
                            sc.on_chunk,
                        )
                        .await;
                    let post_api_ctx = match &stream_collect {
                        Ok(StreamCollectOutcome::Complete(resp)) => {
                            if let Some(lfr) = last_finish_reason.as_deref_mut() {
                                *lfr = resp.finish_reason.clone();
                            }
                            serde_json::json!({
                                "attempt": inner_attempt,
                                "model": sc.active_model,
                                "stream": true,
                                "ok": true,
                                "finish_reason": resp.finish_reason,
                            })
                        }
                        Ok(StreamCollectOutcome::Interrupted(_, _)) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": sc.active_model,
                            "stream": true,
                            "ok": false,
                            "interrupted": true,
                        }),
                        Err(err) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": sc.active_model,
                            "stream": true,
                            "ok": false,
                            "error": err.to_string(),
                        }),
                    };
                    let _ = self.invoke_hook(HookType::PostApiRequest, &post_api_ctx);
                    // collect_stream_llm_response 自身 Err（provider 构建/凭据错误）
                    // 转 graceful 中断，保证 ctx 消息被返回并持久化。
                    let stream_outcome = match stream_collect {
                        Ok(outcome) => outcome,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "collect_stream_llm_response failed — treating as graceful interrupt for persistence"
                            );
                            return TurnLlmOutcome::Terminate(TurnLlmTermination::Errored(e.to_string()));
                        }
                    };
                    match stream_outcome {
                        StreamCollectOutcome::Complete(resp) => resp,
                        StreamCollectOutcome::Interrupted(partial, captured_error) => {
                            cost_guard.record_usage(
                                partial.usage.as_ref(),
                                partial.model.as_str(),
                                &self.config,
                            );
                            // 中途断流（stream_error / 用户中断）：partial 里未执行的
                            // tool_calls 永远不会有 tool result 配对，重放会触发
                            // strict provider 400。strip 掉，正文保留，下一轮重调工具。
                            let errored = partial.finish_reason.as_deref() == Some("stream_error");
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
                            return TurnLlmOutcome::Terminate(
                                TurnLlmTermination::StreamInterrupted {
                                    errored,
                                    captured_error,
                                },
                            );
                        }
                    }
                }
                _ => {
                    // 非流式（run 全程 / run_stream 的降级轮）。
                    match self
                        .call_llm_with_retry(ctx, tool_schemas, turn_runtime_route)
                        .await
                    {
                        Ok(resp) => resp,
                        Err(AgentError::Interrupted { .. }) => {
                            return TurnLlmOutcome::Terminate(TurnLlmTermination::UserInterrupted);
                        }
                        Err(e) => {
                            // 上下文溢出自愈（2026-11-08，cx-chaoxing-progress 任务死因）：
                            // 压力模型（估算 tokens / max_context_tokens）没到预警线时，
                            // 供应商已因真实窗口溢出 400；旧路径直接 Terminate 让任务
                            // 自杀。识别 ContextOverflow → 强制同步压缩 → 重试一次；
                            // 压缩后仍失败才按原路径优雅终止。
                            if crate::agent_helpers::classify_error(&e.to_string())
                                == crate::agent_helpers::ErrorClass::ContextOverflow
                            {
                                tracing::warn!(
                                    error = %e,
                                    "provider rejected context length — forcing compression and retrying once"
                                );
                                self.force_compress_now(ctx).await;
                                match self
                                    .call_llm_with_retry(ctx, tool_schemas, turn_runtime_route)
                                    .await
                                {
                                    Ok(resp) => resp,
                                    Err(retry_err) => {
                                        tracing::warn!(
                                            error = %retry_err,
                                            "LLM call failed after forced compression — treating as graceful interrupt for persistence"
                                        );
                                        return TurnLlmOutcome::Terminate(
                                            TurnLlmTermination::Errored(retry_err.to_string()),
                                        );
                                    }
                                }
                            } else {
                                if stream.is_some() {
                                    tracing::warn!(
                                        error = %e,
                                        "LLM call failed (stream retry path) — treating as graceful interrupt for persistence"
                                    );
                                } else {
                                    tracing::warn!(
                                        error = %e,
                                        "LLM call failed (non-stream path) — treating as graceful interrupt for persistence"
                                    );
                                }
                                return TurnLlmOutcome::Terminate(TurnLlmTermination::Errored(e.to_string()));
                            }
                        }
                    }
                }
            };
            // 只有流式路径需要 inner_attempt 递增（纯非流式 run() 无需计数）。
            if stream.is_some() {
                inner_attempt = inner_attempt.saturating_add(1);
            }
            if let Some(ref u) = r.usage {
                turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
            }
            if let Some(ref u) = turn_usage_acc {
                self.emit_usage_update(u);
            }
            match self.classify_post_llm_response(r, &mut inner_thinking, &mut inner_empty, ctx) {
                PostLlmAction::Break(resp) => {
                    return TurnLlmOutcome::Response { response: resp, turn_usage_acc };
                }
                PostLlmAction::RetryEmpty => continue,
            }
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
        #[allow(unused_mut)] // codex_ack_continuations is read-only in run() today but mut is fine for future edits
        let RunSetup {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
            mut state,
            mut empty_stop_retry_count,
            mut post_tool_summarize_count,
            mut turn_budget,
        } = self.prepare_run_state(messages, tools.as_ref(), "run").await;
        // LoopState owns total_turns; aliased here for hot-path readability.
        let mut total_turns: u32 = state.total_turns;
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();
        // 目标驱动自动续跑的总次数（硬上限见 MAX_GOAL_CONTINUATIONS）。
        let mut goal_continuations: u32 = 0;

        loop {
            // turn transition watchdog（原 run/run_stream 各一份的 48 行块，
            // 现收进 TurnBudget；超时终止走 finalize_transition_stall）。
            if let Some((elapsed_secs, deadline_secs)) = turn_budget.stalled() {
                tracing::warn!(
                    target: "hermix.agent",
                    turn = total_turns,
                    elapsed_secs,
                    deadline_secs,
                    "turn_transition_stall: no progress since post-tool-iteration; \
                     emitting Done to recover WS client"
                );
                return Ok(self.finalize_transition_stall(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    &state,
                    session_id,
                    review_memory_at_end,
                    session_started_hooks_fired,
                    persist_user_idx,
                    Some("turn_transition_stall".to_string()),
                ));
            }
            turn_budget.disarm();

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
            // 零新信息守卫命中后，本轮请求不再携带工具 schema —— 模型
            // 只能输出结论，空转循环到此为止（见 `RepeatGuard`）。
            let turn_tools: &[ToolSchema] = if state.repeat_guard.tools_disabled() {
                &[]
            } else {
                &tool_schemas
            };
            let (response, turn_usage_acc) = match self
                .call_turn_llm(
                    &mut ctx,
                    turn_tools,
                    turn_runtime_route.as_ref(),
                    &mut state.cost_guard,
                    None,
                    None,
                )
                .await
            {
                TurnLlmOutcome::Response { response, turn_usage_acc } => (response, turn_usage_acc),
                TurnLlmOutcome::Terminate(t) => {
                    return Ok(self.finish_terminated_turn(
                        &ctx,
                        t,
                        total_turns,
                        &tool_errors,
                        &state.cost_guard,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
            };
            let api_elapsed = api_start.elapsed().as_millis() as u64;
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
                        // 目标驱动：出现可见文本不等于任务完成。默认落回
                        // decide_empty_response 让模型自检目标、必要时继续干活；
                        // 只有「已推过一次且模型仍然只说话」或自动续跑预算耗尽
                        // 才真正收尾（判定见 should_goal_continue）。
                        let should_autonomous_nudge = self.should_goal_continue(
                            autonomous_nudge_count,
                            goal_continuations,
                            total_turns,
                            tool_schemas.is_empty(),
                        );
                        if !should_autonomous_nudge {
                            // 停止必须可归因（2026-10-30）：这条早退以前一条
                            // 日志都不打，"为什么这一轮停了"只能靠反推代码。
                            tracing::info!(
                                turn = total_turns,
                                exit = "finalize_text_only_turn",
                                blocker = self
                                    .goal_continue_blocker(
                                        autonomous_nudge_count,
                                        goal_continuations,
                                        total_turns,
                                        tool_schemas.is_empty(),
                                    )
                                    .unwrap_or("unknown"),
                                goal_continuations,
                                autonomous_nudge_count,
                                "finalize: LLM produced visible text without tool calls"
                            );
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
                            // 纯文本自然收尾也必须走会话终态持久化：此前这里
                            // 漏调 persist_session_terminal，spawn_background_review
                            // 不触发 → iters_since_skill 阈值复位永不发生
                            // （parity fixture `skill_counter_hits_threshold_and_resets`
                            // 实测复现；run_stream 同款分支本就有此调用）。
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
                        // Fall through：由 decide_empty_response 发出目标续跑
                        // nudge，把模型推回去继续干活。
                        goal_continuations = goal_continuations.saturating_add(1);
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
            let raw_tool_call_count = tool_calls.as_ref().map(|v| v.len()).unwrap_or(0);
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
                // 单一判据（2026-10-30）：与上方 Finalize 分支共用
                // `goal_continue_blocker`。此处的 `nudge_eligible` 以前额外要求
                // `!history_includes_tool`，长任务里恒为 false —— 上一处刚判
                // 「该继续」，这里又判「该收尾」，nudge 被静默吞掉。
                let nudge_eligible = self
                    .goal_continue_blocker(
                        autonomous_nudge_count,
                        goal_continuations,
                        total_turns,
                        tool_schemas.is_empty(),
                    )
                    .is_none();
                if has_visible && !nudge_eligible {
                    // 文案曾把「本轮没有任何 tool_call」也写成「所有工具名都是空的」，
                    // 读日志会把人带偏到 MiniMax 空名缺陷上。按有没有真的丢弃空名分开记。
                    let dropped_unnamed = raw_tool_call_count > 0;
                    tracing::info!(
                        turn = total_turns,
                        exit = if dropped_unnamed {
                            "finalize_unnamed_tool_calls"
                        } else {
                            "finalize_no_tool_calls"
                        },
                        dropped = raw_tool_call_count,
                        blocker = self
                            .goal_continue_blocker(
                                autonomous_nudge_count,
                                goal_continuations,
                                total_turns,
                                tool_schemas.is_empty(),
                            )
                            .unwrap_or("unknown"),
                        goal_continuations,
                        autonomous_nudge_count,
                        history_includes_tool,
                        "finalize: no dispatchable tool call this turn"
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
            // 模型又开始调工具了 —— 目标续跑计数清零，让它每次收尾后都
            // 能再被推回一次（见 should_goal_continue）。
            autonomous_nudge_count = 0;

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
            turn_budget.arm();
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

        let RunSetup {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
            mut state,
            mut empty_stop_retry_count,
            mut post_tool_summarize_count,
            mut turn_budget,
        } = self.prepare_run_state(messages, tools.as_ref(), "run_stream").await;
        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();
        // LoopState owns total_turns; aliased here for hot-path readability.
        let mut total_turns: u32 = state.total_turns;
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        // 目标驱动自动续跑的总次数（硬上限见 MAX_GOAL_CONTINUATIONS）。
        let mut goal_continuations: u32 = 0;

        loop {
            // turn transition watchdog（同 run()：TurnBudget 单一实现）。
            if let Some((elapsed_secs, deadline_secs)) = turn_budget.stalled() {
                tracing::warn!(
                    target: "hermix.agent",
                    turn = total_turns,
                    elapsed_secs,
                    deadline_secs,
                    "turn_transition_stall: no progress since post-tool-iteration; \
                     emitting Done to recover WS client"
                );
                return Ok(self.finalize_transition_stall(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    &state,
                    session_id,
                    review_memory_at_end,
                    session_started_hooks_fired,
                    persist_user_idx,
                    Some(
                        last_finish_reason
                            .clone()
                            .unwrap_or_else(|| "turn_transition_stall".to_string()),
                    ),
                ));
            }
            turn_budget.disarm();

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
            // 同 `run()`：零新信息命中后不再下发工具 schema。
            let turn_tools: &[ToolSchema] = if state.repeat_guard.tools_disabled() {
                &[]
            } else {
                &tool_schemas
            };
            let (response, turn_usage_acc) = match self
                .call_turn_llm(
                    &mut ctx,
                    turn_tools,
                    turn_runtime_route.as_ref(),
                    &mut state.cost_guard,
                    Some(StreamCallCtx {
                        active_model: &active_model,
                        on_chunk: &*on_chunk,
                    }),
                    Some(&mut last_finish_reason),
                )
                .await
            {
                TurnLlmOutcome::Response { response, turn_usage_acc } => (response, turn_usage_acc),
                TurnLlmOutcome::Terminate(t) => {
                    return Ok(self.finish_terminated_turn(
                        &ctx,
                        t,
                        total_turns,
                        &tool_errors,
                        &state.cost_guard,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
            };
            let api_elapsed_ms = api_start.elapsed().as_millis() as u64;
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
                        // 目标驱动：出现可见文本不等于任务完成。默认落回
                        // decide_empty_response 让模型自检目标、必要时继续干活；
                        // 只有「已推过一次且模型仍然只说话」或自动续跑预算耗尽
                        // 才真正收尾（判定见 should_goal_continue）。
                        let should_autonomous_nudge = self.should_goal_continue(
                            autonomous_nudge_count,
                            goal_continuations,
                            total_turns,
                            tool_schemas.is_empty(),
                        );
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
                            tracing::info!(
                                turn = total_turns,
                                exit = "finalize_text_only_turn",
                                blocker = self
                                    .goal_continue_blocker(
                                        autonomous_nudge_count,
                                        goal_continuations,
                                        total_turns,
                                        tool_schemas.is_empty(),
                                    )
                                    .unwrap_or("unknown"),
                                goal_continuations,
                                autonomous_nudge_count,
                                "finalize: LLM produced visible text without tool calls"
                            );
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
                        // Fall through：由 decide_empty_response 发出目标续跑
                        // nudge，把模型推回去继续干活。
                        goal_continuations = goal_continuations.saturating_add(1);
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

            let raw_tool_call_count = tool_calls.as_ref().map(|v| v.len()).unwrap_or(0);
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
                // 单一判据（与 run() 同一实现）：`goal_continue_blocker`
                // 决定了这里是不是该把模型推回去；以前这里用的是带
                // `!history_includes_tool` 的独立判据，长任务里恒为 false。
                let nudge_eligible = self
                    .goal_continue_blocker(
                        autonomous_nudge_count,
                        goal_continuations,
                        total_turns,
                        tool_schemas.is_empty(),
                    )
                    .is_none();
                if has_visible && !nudge_eligible {
                    let dropped_unnamed = raw_tool_call_count > 0;
                    tracing::info!(
                        turn = total_turns,
                        exit = if dropped_unnamed {
                            "finalize_unnamed_tool_calls"
                        } else {
                            "finalize_no_tool_calls"
                        },
                        dropped = raw_tool_call_count,
                        blocker = self
                            .goal_continue_blocker(
                                autonomous_nudge_count,
                                goal_continuations,
                                total_turns,
                                tool_schemas.is_empty(),
                            )
                            .unwrap_or("unknown"),
                        goal_continuations,
                        autonomous_nudge_count,
                        history_includes_tool,
                        "finalize: no dispatchable tool call this turn"
                    );
                    // 只有真的丢弃了空名 tool_call 才提示用户「工具未解析」；
                    // 模型本来就没调工具的普通收尾不该背这个锅。
                    if dropped_unnamed {
                        self.emit_status(
                            "lifecycle",
                            "工具调用未能解析（模型未返回工具名），本轮已按普通回复结束；可重发继续",
                        );
                    }
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
            // 模型又开始调工具了 —— 目标续跑计数清零，让它每次收尾后都
            // 能再被推回一次（见 should_goal_continue）。
            autonomous_nudge_count = 0;

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
            turn_budget.arm();
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
