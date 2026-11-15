//! Protocol fallback single point — **trust + know** model.
//!
//! ## Design philosophy
//!
//! **Core principle: trust the LLM's judgment, but know its work state.**
//!
//! The LLM's `finish_reason=stop` is a signal. It doesn't carry semantics
//! — "I'm done" vs "I'm stuck" look the same from the wire. Rather than
//! overlaying 5 layers of nudges that second-guess every stop, we
//! **classify the work state** from observable signals and act accordingly.
//!
//! ### WorkState classification
//!
//! When the LLM stops with no tool calls, it is in exactly one of:
//!
//! | State           | Visible text | Tool history | Meaning              | Action         |
//! |-----------------|--------------|--------------|----------------------|----------------|
//! | `Answered`      | yes          | any          | Gave a real answer   | Finalize       |
//! | `StoppedSilently` | no         | yes          | Chose silence        | Finalize       |
//! | `EmptyStop`     | no           | no           | Likely API error     | Retry once     |
//! | `GaveUp`        | yes          | yes          | Stage done, goal?    | Goal continuation |
//!
//! ### What we removed (and why)
//!
//! The old code had 5 nudge layers, each a form of "don't trust the LLM's
//! stop":
//! 1. ~~Post-tool nudge (×3)~~ — "LLM was silent after tools, push it"
//! 2. ~~Permission-giveup nudge (×3)~~ — "LLM said no permission, correct it"
//! 3. ~~Tool-failure-retry nudge (×3)~~ — "LLM stopped after a failed tool"
//! 4. ~~Completion-verify nudge (×1)~~ — "LLM said done, verify it"
//! 5. Goal continuation (once per tool chain) — KEPT: it is not a trust
//!    override but the definition of "done". A visible reply means this
//!    *stage* ended, not that the user's objective was met; the model is
//!    pushed back once to self-check against evidence and keep going.
//!
//! Each removed nudge added 1+ LLM API calls (cost + latency) and relied
//! on brittle string-matching heuristics (e.g. `looks_like_permission_giveup`).
//! The new model trusts the LLM's stop signal and uses `reasoning_content`
//! as the visible fallback when the LLM chose silence.
//!
//! ### What we kept
//!
//! - **Codex ack continuation** — protocol-level need for the Responses API
//! - **Goal continuation** — once per tool chain (`autonomous_nudge_count`
//!   is reset whenever the model calls a tool again), plus a hard cap in
//!   `run_loop::MAX_GOAL_CONTINUATIONS`
//! - **EmptyStop retry** — the one case where we don't trust: an empty
//!   response with no tool history is most likely an API glitch, not a
//!   deliberate LLM choice. One retry, then Finalize.

use crate::python_alignment::{looks_like_codex_intermediate_ack, CODEX_CONTINUE_USER_MESSAGE};
use crate::ApiMode;
use hermix_core::Message;

/// Hard cap on Codex-style ack continuations.
pub const CODEX_MAX_ACK_CONTINUATIONS: u32 = 2;

// ---------------------------------------------------------------------------
// Transient nudge texts — these injected `Message::system` control messages
// are one-shot loop-steering instructions, NOT conversation content. They
// must never reach the persisted transcript: a persisted nudge (a) cuts
// `merge_consecutive_assistant_messages` mid-run so two LLM outputs of one
// run reload as two separate assistant bubbles ("自己重复回答"), and (b)
// pollutes every later turn's LLM context with a stale "you have not yet
// started working" instruction. `is_transient_nudge_text` is the single
// recognition point; `local_agent_service` strips them before persisting.
// ---------------------------------------------------------------------------
pub const AUTONOMOUS_NUDGE_VISIBLE_PREFIX: &str =
    "You produced a text reply but have not yet started working on the task.";
pub const AUTONOMOUS_NUDGE_SILENT_PREFIX: &str =
    "You have not yet completed the user's task.";
pub const EMPTY_STOP_NUDGE_PREFIX: &str =
    "Your previous response was empty. Please provide a response or call a tool to work on the task.";
pub const POST_TOOL_SUMMARIZE_NUDGE_PREFIX: &str =
    "你刚才调用了工具但本轮没有输出任何文字。";

/// Recognize the system-role control messages injected by
/// `decide_empty_response`'s nudge branches. Matched by exact prefix —
/// the texts are defined right here, so prefix and message stay in sync.
pub fn is_transient_nudge_text(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        AUTONOMOUS_NUDGE_VISIBLE_PREFIX,
        AUTONOMOUS_NUDGE_SILENT_PREFIX,
        EMPTY_STOP_NUDGE_PREFIX,
        POST_TOOL_SUMMARIZE_NUDGE_PREFIX,
    ];
    PREFIXES.iter().any(|p| text.starts_with(p))
}

/// Max retries for an `EmptyStop` (empty response, no tool history).
/// One retry covers transient API glitches; a second empty response
/// means the LLM genuinely has nothing to say.
pub const MAX_EMPTY_STOP_RETRIES: u32 = 1;

/// Max post-tool summary nudges. When the LLM called tools this turn
/// but produced no visible text (`StoppedSilently`), push it once to
/// write a task summary for the user; a second silent stop is
/// respected (the model genuinely has nothing to say and we must not
/// loop forever).
pub const MAX_POST_TOOL_SUMMARIZE: u32 = 1;

/// What should the agent loop do when the LLM responded with no tool
/// calls?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyResponseAction {
    /// Inject a user message (e.g. Codex continue) and start the next
    /// turn immediately.
    ContinueWithUserMsg(String),
    /// Inject a system nudge (autonomous mode, pre-tool) and start the
    /// next turn.
    ContinueWithSystemNudge(String),
    /// Hand control back to the loop, which will run the final memory
    /// sync and return the result.
    Finalize,
}

/// The LLM's work state when it stops without tool calls.
///
/// Derived from two observable signals:
/// - Does the assistant message have visible text (after stripping `<think>` blocks)?
/// - Has the conversation history already included a tool call/result?
///
/// This enum is **purely informational** — it's logged for observability
/// and used to drive the single retry decision. It does NOT override the
/// LLM's stop signal (except for `EmptyStop` → one retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// LLM produced visible text → it gave a real answer. Trust and finalize.
    Answered,
    /// LLM produced no visible text but tools ran earlier → it chose silence.
    /// Trust the stop. `reasoning_content` is the visible fallback.
    StoppedSilently,
    /// LLM produced no visible text AND no tools have run → likely an API
    /// glitch. One retry, then finalize.
    EmptyStop,
    /// LLM produced visible text after tools ran → it decided to stop
    /// (could be a summary, a give-up, or anything in between). Trust it.
    /// Same action as `Answered` but distinct state for observability.
    GaveUp,
}

impl WorkState {
    /// Classify the LLM's work state from observable signals.
    pub fn classify(has_visible_text: bool, history_includes_tool: bool) -> Self {
        match (has_visible_text, history_includes_tool) {
            (true, true) => WorkState::GaveUp,
            (true, false) => WorkState::Answered,
            (false, true) => WorkState::StoppedSilently,
            (false, false) => WorkState::EmptyStop,
        }
    }

    /// Short label for tracing.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkState::Answered => "answered",
            WorkState::StoppedSilently => "stopped_silently",
            WorkState::EmptyStop => "empty_stop",
            WorkState::GaveUp => "gave_up",
        }
    }
}

impl std::fmt::Display for WorkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// All inputs the decision needs.
#[derive(Debug)]
pub struct EmptyResponseInput<'a> {
    pub assistant_msg: &'a Message,
    pub api_mode: ApiMode,
    pub has_tool_schemas: bool,
    pub history_includes_tool: bool,
    pub task_hint: &'a str,
    pub codex_ack_continuations: u32,
    pub autonomous_mode: bool,
    pub autonomous_nudge_count: u32,
    /// Number of `EmptyStop` retries already attempted this session.
    /// Capped at [`MAX_EMPTY_STOP_RETRIES`].
    pub empty_stop_retry_count: u32,
    /// Number of post-tool summary nudges already sent this session.
    /// Capped at [`MAX_POST_TOOL_SUMMARIZE`].
    pub post_tool_summarize_count: u32,
    pub total_turns: u32,
    pub max_turns: u32,
    pub max_autonomous_nudges: u32,
    pub assistant_has_visible_text: fn(&Message) -> bool,
}

/// The decision, plus counter increments.
#[derive(Debug)]
pub struct EmptyResponseOutput {
    pub action: EmptyResponseAction,
    pub new_codex_ack_continuations: u32,
    pub new_autonomous_nudge_count: u32,
    pub new_empty_stop_retry_count: u32,
    /// Incremented when a post-tool summary nudge is sent. See
    /// [`MAX_POST_TOOL_SUMMARIZE`].
    pub new_post_tool_summarize_count: u32,
    /// The classified work state (for logging / observability).
    pub work_state: WorkState,
}

/// Single authority for "what should the loop do when the LLM produces
/// no tool calls this turn".
///
/// Priority order:
/// 1. Codex ack continuation (protocol need)
/// 2. Autonomous pre-tool nudge (user preference — one shot)
/// 3. EmptyStop retry (API glitch recovery — one shot)
/// 4. Finalize (default — trust the LLM's stop signal)
pub fn decide_empty_response(input: EmptyResponseInput<'_>) -> EmptyResponseOutput {
    let has_visible = (input.assistant_has_visible_text)(input.assistant_msg);
    let work_state = WorkState::classify(has_visible, input.history_includes_tool);

    // 1. Codex ack continuation — protocol-level: the Codex Responses
    // API sometimes emits a planning/ack reply that needs an explicit
    // "continue" user message to proceed. This is NOT a trust override
    // — it's a wire-protocol requirement.
    if input.api_mode == ApiMode::CodexResponses
        && input.has_tool_schemas
        && input.codex_ack_continuations < CODEX_MAX_ACK_CONTINUATIONS
        && looks_like_codex_intermediate_ack(
            input.task_hint,
            input.assistant_msg.content.as_deref().unwrap_or(""),
            input.history_includes_tool,
        )
    {
        tracing::info!(
            work_state = %work_state,
            ack_count = input.codex_ack_continuations + 1,
            "codex_ack_continuation: protocol-level continue"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithUserMsg(
                CODEX_CONTINUE_USER_MESSAGE.to_string(),
            ),
            new_codex_ack_continuations: input.codex_ack_continuations + 1,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_empty_stop_retry_count: input.empty_stop_retry_count,
            new_post_tool_summarize_count: input.post_tool_summarize_count,
            work_state,
        };
    }

    // 2. 目标续跑（goal continuation）—— 出现可见文本 ≠ 任务完成。
    //
    // 旧判据要求 `!history_includes_tool`，只覆盖「模型光说不做」。长任务
    // 一旦动过工具，该判据永久为假，于是「干一段 → 写总结 → 交回用户」成了
    // 唯一出口，用户必须每轮手打「继续」。
    //
    // 2026-10-30：预算改成 `max_autonomous_nudges`（默认 8）。此前这里写死
    // `== 0`，于是 `max_autonomous_nudges` 虽然一路传进本结构体却从未被读，
    // 真实预算恒为「1 次」。模型重新调用工具时该计数会被 `run_loop` 清零，
    // 所以「干活 → 收尾 → 被推回 → 再干活」可以一路循环到目标完成；连续推满
    // 预算仍然只说话则放行收尾（不空转）。
    //
    // `StoppedSilently`（工具跑完但一个字没说）不走这里 —— 交给下面的
    // post-tool summarize，语义更贴切。
    if input.autonomous_mode
        && input.has_tool_schemas
        && (has_visible || !input.history_includes_tool)
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.autonomous_nudge_count < input.max_autonomous_nudges
    {
        let turns_info = if input.max_turns > 0 {
            format!("{} of {} used", input.total_turns, input.max_turns)
        } else {
            format!("{} turns used", input.total_turns)
        };
        let nudge = if has_visible {
            format!(
                "{} \
                 收尾之前先自检：用户交办的**整个目标**是否已经全部达成（不是这一阶段，\
                 是整个任务）。判定依据只能是实据——文件已写入、命令已执行、测试已通过、\
                 界面已回到预期状态；计划、待办清单、「应该没问题」都不算数。\
                 未达成 → 直接继续执行下一步（缺信息就用工具去取，不要问用户），\
                 不要用「下一步我会…」的预告代替执行。\
                 已达成 → 给出最终结论（做成了什么、证据是什么、还剩什么风险），\
                 并且不要用「要不要继续 / 这轮先到这 / 需要我接着做吗」这类问句收尾。\
                 （{}）",
                AUTONOMOUS_NUDGE_VISIBLE_PREFIX, turns_info
            )
        } else {
            format!(
                "{} \
                 工具可用且还有回合（{}）。若用户交办的是实际任务，不要停在这里——\
                 继续用工具推进（缺信息自己取，不要回头问用户）；\
                 若确实没有任务可做，给出正常的对话回复并结束。",
                AUTONOMOUS_NUDGE_SILENT_PREFIX, turns_info
            )
        };
        tracing::info!(
            work_state = %work_state,
            nudge_count = input.autonomous_nudge_count + 1,
            total_turns = input.total_turns,
            "goal_continuation: nudging LLM to self-check the objective and keep going"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count + 1,
            new_empty_stop_retry_count: input.empty_stop_retry_count,
            new_post_tool_summarize_count: input.post_tool_summarize_count,
            work_state,
        };
    }

    // 3. EmptyStop retry — the ONE case where we don't trust the LLM's
    // stop signal. An empty response (no visible text, no tool history)
    // is most likely an API glitch (timeout, partial response, rate
    // limit) rather than a deliberate LLM choice. One retry; if the
    // LLM still produces nothing, we respect that and finalize.
    // Fires AFTER autonomous nudge: if autonomous mode already nudged
    // and the LLM still returned empty, this retry gives one more
    // chance before finalizing.
    if work_state == WorkState::EmptyStop
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.empty_stop_retry_count < MAX_EMPTY_STOP_RETRIES
    {
        tracing::info!(
            work_state = %work_state,
            retry = input.empty_stop_retry_count + 1,
            max = MAX_EMPTY_STOP_RETRIES,
            total_turns = input.total_turns,
            "empty_stop_retry: empty response with no tool history — retrying (likely API glitch)"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(
                EMPTY_STOP_NUDGE_PREFIX.to_string(),
            ),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_empty_stop_retry_count: input.empty_stop_retry_count + 1,
            new_post_tool_summarize_count: input.post_tool_summarize_count,
            work_state,
        };
    }

    // 4. Post-tool silent summary nudge — the LLM called tools this
    // turn but produced NO visible text. Trusting the stop here would
    // end the turn with zero words for the user: "任务结束但没有任何
    // 总结" (tool calls visible, conclusion invisible). Push the model
    // once to write ANY concluding reply — the content/tone/detail is
    // entirely the LLM's choice, we only require that it speaks; a
    // second silent stop is respected via the cap.
    if work_state == WorkState::StoppedSilently
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.post_tool_summarize_count < MAX_POST_TOOL_SUMMARIZE
    {
        tracing::info!(
            work_state = %work_state,
            nudge = input.post_tool_summarize_count + 1,
            max = MAX_POST_TOOL_SUMMARIZE,
            total_turns = input.total_turns,
            "post_tool_summarize: tools ran but no visible text — nudging for a task summary"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(
                format!(
                    "{}请给用户一个总结性的回复（内容、详略、语气由你决定）。不要只调用工具而不说话。",
                    POST_TOOL_SUMMARIZE_NUDGE_PREFIX
                ),
            ),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_empty_stop_retry_count: input.empty_stop_retry_count,
            new_post_tool_summarize_count: input.post_tool_summarize_count + 1,
            work_state,
        };
    }

    // 5. Default: trust the LLM's stop signal and finalize.
    // The LLM chose to stop — we respect that. If it produced no
    // visible text, the `local_agent_service` falls back to
    // `reasoning_content` as the visible reply so the user always
    // sees something.
    tracing::info!(
        work_state = %work_state,
        has_visible_text = has_visible,
        history_includes_tool = input.history_includes_tool,
        total_turns = input.total_turns,
        "finalize: trusting LLM's stop signal"
    );
    EmptyResponseOutput {
        action: EmptyResponseAction::Finalize,
        new_codex_ack_continuations: input.codex_ack_continuations,
        new_autonomous_nudge_count: input.autonomous_nudge_count,
        new_empty_stop_retry_count: input.empty_stop_retry_count,
        new_post_tool_summarize_count: input.post_tool_summarize_count,
        work_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    fn make_assistant_msg(content: Option<&str>) -> Message {
        let mut m = Message::assistant(content.unwrap_or("").to_string());
        // Ensure empty string becomes None-like for the test
        if content.is_none() {
            m.content = None;
        }
        m
    }

    fn visible_text_fn(m: &Message) -> bool {
        let Some(c) = m.content.as_deref() else {
            return false;
        };
        !c.trim().is_empty()
    }

    fn base_input<'a>(assistant: &'a Message) -> EmptyResponseInput<'a> {
        EmptyResponseInput {
            assistant_msg: assistant,
            api_mode: ApiMode::ChatCompletions,
            has_tool_schemas: true,
            history_includes_tool: true,
            task_hint: "do the task",
            codex_ack_continuations: 0,
            autonomous_mode: false,
            autonomous_nudge_count: 0,
            empty_stop_retry_count: 0,
            post_tool_summarize_count: 0,
            total_turns: 3,
            max_turns: 30,
            max_autonomous_nudges: 8,
            assistant_has_visible_text: visible_text_fn,
        }
    }

    // ===== WorkState classification tests =====

    #[test]
    fn work_state_answered_when_visible_text_no_tools() {
        let _m = make_assistant_msg(Some("Here's the answer."));
        assert_eq!(
            WorkState::classify(true, false),
            WorkState::Answered
        );
    }

    #[test]
    fn work_state_gave_up_when_visible_text_with_tools() {
        assert_eq!(
            WorkState::classify(true, true),
            WorkState::GaveUp
        );
    }

    #[test]
    fn work_state_stopped_silently_when_no_text_with_tools() {
        assert_eq!(
            WorkState::classify(false, true),
            WorkState::StoppedSilently
        );
    }

    #[test]
    fn work_state_empty_stop_when_no_text_no_tools() {
        assert_eq!(
            WorkState::classify(false, false),
            WorkState::EmptyStop
        );
    }

    // ===== transient nudge recognition tests =====

    /// 持久化 strip 依赖前缀识别。四段 nudge 文本与常量必须互认：
    /// 文本改了没同步前缀 → nudge 落库 → merge 被切断 → 重复气泡回归。
    #[test]
    fn is_transient_nudge_text_recognizes_all_four_nudges() {
        assert!(is_transient_nudge_text(
            "You produced a text reply but have not yet started working on the task. \
             Tools are available and more turns remain (1 of 30)."
        ));
        assert!(is_transient_nudge_text(
            "You have not yet completed the user's task. \
             Tools are available and more turns remain (1 of 30)."
        ));
        assert!(is_transient_nudge_text(EMPTY_STOP_NUDGE_PREFIX));
        assert!(is_transient_nudge_text(
            "你刚才调用了工具但本轮没有输出任何文字。请给用户一个总结性的回复（内容、详略、语气由你决定）。不要只调用工具而不说话。"
        ));
    }

    #[test]
    fn is_transient_nudge_text_rejects_real_conversation() {
        // 用户/模型的正常对话内容绝不能被 strip 掉。
        assert!(!is_transient_nudge_text("你好！我是 Hermix，有什么可以帮你的？"));
        assert!(!is_transient_nudge_text("上一条已经问过了 —— 还没收到你的需求。"));
        assert!(!is_transient_nudge_text(""));
    }

    // ===== Finalize tests (trust model) =====

    #[test]
    fn answered_finalizes_immediately() {
        // LLM gave a visible answer in a non-tool Q&A session → finalize.
        let m = make_assistant_msg(Some("The capital of France is Paris."));
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.work_state, WorkState::Answered);
    }

    #[test]
    fn gave_up_finalizes_immediately() {
        // LLM produced visible text after tools, chose to stop → trust it.
        let m = make_assistant_msg(Some("20个子目录（车辆），无文件。"));
        let out = decide_empty_response(base_input(&m));
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.work_state, WorkState::GaveUp);
    }

    #[test]
    fn gave_up_finalizes_on_permission_text() {
        // Old code would nudge with "NOT in a sandbox". New code trusts
        // the LLM's judgment — if it says "no permission", that's its call.
        let m = make_assistant_msg(
            Some("I don't have permission to access the C: drive in this sandbox."),
        );
        let out = decide_empty_response(base_input(&m));
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.work_state, WorkState::GaveUp);
    }

    #[test]
    fn gave_up_finalizes_on_task_complete_claim() {
        // Old code would fire a completion-verify nudge. New code trusts
        // the LLM's "done" declaration.
        let m = make_assistant_msg(Some("All files renamed. Task complete."));
        let out = decide_empty_response(base_input(&m));
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.work_state, WorkState::GaveUp);
    }

    #[test]
    fn stopped_silently_gets_single_post_tool_summarize_nudge() {
        // LLM went silent after tools — implementation nudges once for a
        // concluding reply (content left to the LLM); a second silent stop
        // is respected via MAX_POST_TOOL_SUMMARIZE cap.
        let m = make_assistant_msg(None);
        let out = decide_empty_response(base_input(&m));
        assert!(matches!(
            out.action,
            EmptyResponseAction::ContinueWithSystemNudge(_)
        ));
        assert_eq!(out.work_state, WorkState::StoppedSilently);
    }

    // ===== EmptyStop retry tests =====

    #[test]
    fn empty_stop_retries_once_when_no_text_no_tools() {
        // Empty response with no tool history → likely API glitch, retry once.
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        assert_eq!(out.work_state, WorkState::EmptyStop);
        assert_eq!(out.new_empty_stop_retry_count, 1);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("empty"),
                    "retry nudge should mention empty response, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
    }

    #[test]
    fn empty_stop_finalizes_after_max_retries() {
        // After MAX_EMPTY_STOP_RETRIES, respect the LLM's silence.
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        input.empty_stop_retry_count = MAX_EMPTY_STOP_RETRIES;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.work_state, WorkState::EmptyStop);
    }

    #[test]
    fn empty_stop_does_not_retry_when_history_has_tools() {
        // With tool history, silence is StoppedSilently (trust), not EmptyStop.
        // Implementation issues one post-tool-summarize nudge (not a retry).
        let m = make_assistant_msg(None);
        let input = base_input(&m); // history_includes_tool = true
        let out = decide_empty_response(input);
        assert_eq!(out.work_state, WorkState::StoppedSilently);
        assert!(matches!(
            out.action,
            EmptyResponseAction::ContinueWithSystemNudge(_)
        ));
    }

    // ===== Codex ack continuation tests =====

    #[test]
    fn codex_ack_continuation_fires_on_codex_api_with_planning_reply() {
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithUserMsg(text) => {
                assert!(text.contains("Continue now"));
            }
            other => panic!("expected ContinueWithUserMsg, got {:?}", other),
        }
        assert_eq!(out.new_codex_ack_continuations, 1);
    }

    #[test]
    fn codex_ack_caps_at_max() {
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        input.codex_ack_continuations = CODEX_MAX_ACK_CONTINUATIONS;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    // ===== Autonomous pre-tool nudge tests =====

    #[test]
    fn autonomous_nudge_fires_when_no_tools_yet() {
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("继续用工具推进"),
                    "silent nudge should push for tool work, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_autonomous_nudge_count, 1);
    }

    #[test]
    fn autonomous_nudge_fires_with_visible_text_when_no_tools_yet() {
        let m = make_assistant_msg(Some("好的，我来帮你完成这个任务。"));
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        input.autonomous_nudge_count = 0;
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("整个目标"),
                    "visible-text nudge should ask for an objective self-check, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_autonomous_nudge_count, 1);
    }

    #[test]
    fn goal_continuation_fires_after_tools_were_used() {
        // 目标驱动：用过工具之后出现可见文本，不再直接收尾 —— 先自检目标。
        // 旧行为（`!history_includes_tool`）会让长任务每写完一段就结束回合，
        // 用户必须手动打「继续」。
        let m = make_assistant_msg(Some("第一步完成，接下来处理第二步。"));
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        input.autonomous_nudge_count = 0;
        let out = decide_empty_response(input);
        assert_eq!(out.work_state, WorkState::GaveUp);
        assert_eq!(out.new_autonomous_nudge_count, 1);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("整个目标"),
                    "continuation must ask for an objective self-check, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
    }

    #[test]
    fn goal_continuation_runs_until_nudge_budget_exhausted() {
        // 预算内继续推；推满 `max_autonomous_nudges` 才放行收尾（不空转）。
        let m = make_assistant_msg(Some("全部完成了。"));
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        input.autonomous_nudge_count = 1;
        let out = decide_empty_response(input);
        assert_eq!(out.new_autonomous_nudge_count, 2);
        assert!(matches!(
            out.action,
            EmptyResponseAction::ContinueWithSystemNudge(_)
        ));

        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        let budget = input.max_autonomous_nudges;
        input.autonomous_nudge_count = budget;
        let out = decide_empty_response(input);
        assert_eq!(out.new_autonomous_nudge_count, budget);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    #[test]
    fn autonomous_nudge_respects_its_budget() {
        // 预算内第二次仍然推；EmptyStop retry 只接手预算用尽后的空回复。
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        input.autonomous_nudge_count = 1;
        let out = decide_empty_response(input);
        assert_eq!(out.new_autonomous_nudge_count, 2);
        assert_eq!(out.new_empty_stop_retry_count, 0); // EmptyStop retry NOT consumed
        assert!(matches!(
            out.action,
            EmptyResponseAction::ContinueWithSystemNudge(_)
        ));

        // 预算用尽 + EmptyStop retry 也已用尽 → Finalize
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        let budget = input.max_autonomous_nudges;
        input.autonomous_nudge_count = budget;
        input.empty_stop_retry_count = MAX_EMPTY_STOP_RETRIES;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after nudge budget + EmptyStop retry both exhausted"
        );
    }

    // ===== Priority tests =====

    #[test]
    fn priority_codex_over_everything() {
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        input.autonomous_mode = true;
        let out = decide_empty_response(input);
        assert!(matches!(
            out.action,
            EmptyResponseAction::ContinueWithUserMsg(_)
        ));
    }

    #[test]
    fn priority_autonomous_nudge_over_empty_stop_retry() {
        // When both EmptyStop (no text, no tools) and autonomous mode are
        // active, the autonomous nudge fires first — it's a user preference.
        // The EmptyStop retry is a fallback for non-autonomous mode.
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        // Autonomous nudge fires first
        assert_eq!(out.work_state, WorkState::EmptyStop);
        assert_eq!(out.new_autonomous_nudge_count, 1);
        assert_eq!(out.new_empty_stop_retry_count, 0); // EmptyStop retry NOT consumed
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(text.contains("继续用工具推进"), "should be autonomous nudge, got: {text}");
            }
            other => panic!("expected autonomous nudge, got {:?}", other),
        }
    }

    // ===== No fabrication tests =====

    #[test]
    fn no_patch_blank_bubble_even_with_fallback() {
        // Never fabricate LLM output. If the LLM said nothing, we finalize
        // and let local_agent_service fall back to reasoning_content.
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        input.empty_stop_retry_count = MAX_EMPTY_STOP_RETRIES; // no more retries
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize — never fabricate LLM output"
        );
    }

    #[test]
    fn max_turns_prevents_empty_stop_retry() {
        // When max_turns is reached, no retry — finalize immediately.
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        input.total_turns = 30;
        input.max_turns = 30;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }
}
