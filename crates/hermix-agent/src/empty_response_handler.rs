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
//! | `GaveUp`        | yes          | yes          | Decided to stop      | Finalize       |
//!
//! ### What we removed (and why)
//!
//! The old code had 5 nudge layers, each a form of "don't trust the LLM's
//! stop":
//! 1. ~~Post-tool nudge (×3)~~ — "LLM was silent after tools, push it"
//! 2. ~~Permission-giveup nudge (×3)~~ — "LLM said no permission, correct it"
//! 3. ~~Tool-failure-retry nudge (×3)~~ — "LLM stopped after a failed tool"
//! 4. ~~Completion-verify nudge (×1)~~ — "LLM said done, verify it"
//! 5. Autonomous pre-tool nudge (×1) — KEPT: user explicitly asked for
//!    autonomous mode, this is a user preference, not a trust override.
//!
//! Each removed nudge added 1+ LLM API calls (cost + latency) and relied
//! on brittle string-matching heuristics (e.g. `looks_like_permission_giveup`).
//! The new model trusts the LLM's stop signal and uses `reasoning_content`
//! as the visible fallback when the LLM chose silence.
//!
//! ### What we kept
//!
//! - **Codex ack continuation** — protocol-level need for the Responses API
//! - **Autonomous pre-tool nudge** — user preference (fires once)
//! - **EmptyStop retry** — the one case where we don't trust: an empty
//!   response with no tool history is most likely an API glitch, not a
//!   deliberate LLM choice. One retry, then Finalize.

use crate::python_alignment::{looks_like_codex_intermediate_ack, CODEX_CONTINUE_USER_MESSAGE};
use crate::ApiMode;
use hermix_core::Message;

/// Hard cap on Codex-style ack continuations.
pub const CODEX_MAX_ACK_CONTINUATIONS: u32 = 2;

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

    // 2. Autonomous pre-tool nudge — when autonomous mode is on and the
    // LLM hasn't used any tools yet, push it to start working.
    // This is a USER PREFERENCE (they explicitly enabled autonomous mode),
    // not a trust override. Fires once only.
    // Priority over EmptyStop retry: if the user turned on autonomous
    // mode, they want the LLM nudged to use tools — the "empty response"
    // is exactly the scenario the autonomous nudge is designed for.
    if input.autonomous_mode
        && input.has_tool_schemas
        && !input.history_includes_tool
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.autonomous_nudge_count == 0
    {
        let turns_info = if input.max_turns > 0 {
            format!("{} of {} used", input.total_turns, input.max_turns)
        } else {
            format!("{} turns used", input.total_turns)
        };
        let nudge = if has_visible {
            format!(
                "You produced a text reply but have not yet started working on the task. \
                 Tools are available and more turns remain ({}). \
                 Do NOT stop here — use tools to actually accomplish the task. \
                 If you need information, use a tool to obtain it. \
                 If you need to read or write files, execute commands, or search, \
                 call the appropriate tool now. Resume immediately with a tool call.",
                turns_info
            )
        } else {
            format!(
                "You have not yet completed the user's task. \
                 Tools are available and more turns remain ({}). \
                 Do NOT stop here — continue working. \
                 If you need information, use a tool to obtain it rather than asking the user. \
                 Resume immediately with the next tool call.",
                turns_info
            )
        };
        tracing::info!(
            work_state = %work_state,
            nudge_count = input.autonomous_nudge_count + 1,
            total_turns = input.total_turns,
            "autonomous_pre_tool_nudge: pushing LLM to start using tools"
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
                "Your previous response was empty. Please provide a response or call a tool to work on the task.".to_string(),
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
                "你刚才调用了工具但本轮没有输出任何文字。请给用户一个总结性的回复（内容、详略、语气由你决定）。不要只调用工具而不说话。".to_string(),
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
        let m = make_assistant_msg(Some("Here's the answer."));
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
    fn stopped_silently_finalizes_immediately() {
        // LLM went silent after tools — old code would nudge up to 3 times.
        // New code trusts the stop signal. reasoning_content is the fallback.
        let m = make_assistant_msg(None);
        let out = decide_empty_response(base_input(&m));
        assert_eq!(out.action, EmptyResponseAction::Finalize);
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
        let m = make_assistant_msg(None);
        let input = base_input(&m); // history_includes_tool = true
        let out = decide_empty_response(input);
        assert_eq!(out.work_state, WorkState::StoppedSilently);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
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
                    text.contains("continue working"),
                    "pre-tool nudge should say 'continue working', got: {text}"
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
                    text.contains("tool"),
                    "nudge with visible text should redirect to tools, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_autonomous_nudge_count, 1);
    }

    #[test]
    fn autonomous_nudge_does_not_fire_when_tools_already_used() {
        // After tools have been used, autonomous pre-tool nudge must NOT fire.
        let m = make_assistant_msg(Some("20个子目录（车辆），无文件。"));
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        input.autonomous_nudge_count = 0;
        let out = decide_empty_response(input);
        assert_eq!(out.new_autonomous_nudge_count, 0);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    #[test]
    fn autonomous_nudge_fires_only_once() {
        // After autonomous nudge fired once, it won't fire again.
        // But EmptyStop retry can still fire (it hasn't been consumed yet).
        let m = make_assistant_msg(None);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        input.autonomous_nudge_count = 1;
        let out = decide_empty_response(input);
        // EmptyStop retry fires (autonomous nudge already consumed)
        assert_eq!(out.new_autonomous_nudge_count, 1); // unchanged
        assert_eq!(out.new_empty_stop_retry_count, 1); // EmptyStop retry fires
        assert!(matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)));

        // After BOTH autonomous nudge and EmptyStop retry are exhausted → Finalize
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        input.autonomous_nudge_count = 1;
        input.empty_stop_retry_count = MAX_EMPTY_STOP_RETRIES;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after autonomous nudge + EmptyStop retry both exhausted"
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
                assert!(text.contains("continue working") || text.contains("tool"), "should be autonomous nudge, got: {text}");
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
