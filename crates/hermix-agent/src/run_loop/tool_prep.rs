//! tool-call 批次在真正执行前的公共处理。
//!
//! `run()` 与 `run_stream()` 是两条平行主循环（结构性债务，见 2026-10-25
//! 审查清单 P1-11）。这段「去重 → 修补 → 参数水合 → 统一校验 → 进化计数器
//! → delegate 上限 → PreToolCall 钩子 → 中断检查」在两条路径里逐字重复了
//! 约 100 行，而且已经分叉：`finalize_agent_result` 的 `finish_reason` 在
//! `run()` 侧传 `None`、在 `run_stream()` 侧传 `last_finish_reason` ——
//! 同一个用户可见语义（这轮为什么结束）在两条路径上不同。
//!
//! 抽成单一实现后，两条循环只保留 transport 差异（流式才有的静音过渡通过
//! `before_validate` 钩子注入）。

use std::collections::HashMap;

use super::*;
use crate::ContextManager;

/// 一批 tool-call 处理完的三种去向。
pub(super) enum ToolPrep {
    /// 可以执行：处理后的调用 + `tool_call_id` 映射（流式事件复用同一份）。
    Proceed {
        tool_calls: Vec<ToolCall>,
        tool_call_ids: HashMap<String, String>,
    },
    /// 校验要求重来一轮（`ApplyResult::Continue`）—— 调用方 `continue`。
    SkipTurn,
    /// 本轮到此结束（校验耗尽 / 用户中断）—— 调用方直接返回该结果。
    Abort(Box<AgentResult>),
}

impl AgentLoop {
    /// 执行前处理一批 tool calls。
    ///
    /// `finish_reason` 只在校验耗尽时进入结果；`before_validate` 是流式路径
    /// 注入「静音过渡」的位置（它在去重之后、校验之前触发，历史顺序不变）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_tool_calls(
        &self,
        ctx: &mut ContextManager,
        state: &mut LoopState,
        tool_calls: Vec<ToolCall>,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finish_reason: Option<String>,
        phase_label: &str,
        before_validate: Option<&dyn Fn(&[ToolCall])>,
    ) -> Result<ToolPrep, AgentError> {
        // 1. 去重 + 未知工具名修补 + session_search 参数水合。
        let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
        for tc in &mut tool_calls {
            self.repair_tool_call(tc);
            self.hydrate_session_search_args(tc);
        }

        // 2. 流式路径的静音过渡（家务型工具 + 已有可见文本时不再推流）。
        if let Some(hook) = before_validate {
            hook(&tool_calls);
        }

        // 3. 统一校验（AL-7）。
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
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid tool retries reached ({})",
                        self.config.invalid_tool_call_max_retries
                    ),
                );
                // 名字已经通过 `apply_outcome` 进了系统消息，这里只是变量占位。
                let _ = invalid_names;
            }
            crate::tool_call_validator::ValidationOutcome::RetryInvalidJson => {
                self.emit_status("lifecycle", "Invalid tool JSON arguments — retrying");
            }
            crate::tool_call_validator::ValidationOutcome::FatalInvalidJson { .. } => {
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
            }
            crate::tool_call_validator::ValidationOutcome::Valid => {}
        }
        match crate::tool_call_validator::apply_outcome(ctx, &tool_calls, outcome) {
            crate::tool_call_validator::ApplyResult::Proceed => {}
            crate::tool_call_validator::ApplyResult::Continue => return Ok(ToolPrep::SkipTurn),
            crate::tool_call_validator::ApplyResult::ReturnFatal => {
                // #A（全局修正）：校验耗尽是**可恢复的后端失败**，不是干净完成。
                // `interrupted = true` 让服务端把 Done 标成 interrupted，前端保留
                // 会话（不断 WS、不清 pending approval），而不是静默杀掉这一轮。
                return Ok(ToolPrep::Abort(Box::new(self.finalize_agent_result(
                    ctx,
                    total_turns,
                    tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    true,
                    finish_reason,
                ))));
            }
        }

        // 4. 进化计数器（记忆/技能类工具重置冷却）。
        for tc in &tool_calls {
            if let Ok(mut c) = self.evolution_counters.lock() {
                match tc.function.name.as_str() {
                    "memory" => c.turns_since_memory = 0,
                    "skill_manage" => c.iters_since_skill = 0,
                    _ => {}
                }
            }
        }

        // 5. 并发 delegate_task 上限。
        self.cap_delegates(&mut tool_calls);

        // 6. PreToolCall 钩子 + 前端回调。
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
            // O7.2: 模型向用户提问时立刻推给前端（不等工具执行完）。
            if tc.function.name == "clarify" {
                if let Some(ref cb) = self.callbacks.on_clarify {
                    let question = tc.function.arguments.clone();
                    cb(&question);
                }
            }
        }

        // 7. 用户在工具执行前按下 /stop。
        if self.interrupt.take_interrupt_graceful().is_some() {
            return Ok(ToolPrep::Abort(Box::new(self.graceful_interrupt_result(
                ctx,
                total_turns,
                tool_errors,
                state.cost_guard.accumulated_usage.clone(),
                state.cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
            ))));
        }

        tracing::debug!(
            phase = %LoopPhase::ToolValidation,
            turn = total_turns,
            calls = tool_calls.len(),
            "{}: tool calls prepared",
            phase_label
        );
        Ok(ToolPrep::Proceed {
            tool_calls,
            tool_call_ids,
        })
    }
}
