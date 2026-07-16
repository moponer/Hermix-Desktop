//! Protocol fallback single point (C2 refactor).
//!
//! ## Design philosophy (revised 2026-07-09)
//!
//! The normal flow is simple:
//! 1. LLM thinks → `</think>` goes to collapsible block in the UI
//! 2. LLM outputs visible text → rendered as Markdown for the user
//! 3. LLM calls tools → results go into context → next turn LLM naturally summarizes
//!
//! The system prompt already contains `TOOL_EXECUTION_SUMMARY_GUIDANCE`,
//! `STOPPING_SUMMARY_GUIDANCE`, and `THINKING_DISCIPLINE_GUIDANCE` — these
//! instruct the LLM to produce visible text after tool execution. We trust
//! the LLM to follow these instructions.
//!
//! This module handles the **rare edge cases** where the LLM stops without
//! tool calls and we need to decide what to do:
//!
//! 1. **Codex ack continuation** — protocol-level need for the Codex
//!    Responses API when the model emits a planning reply.
//! 2. **Autonomous pre-tool nudge** — when autonomous mode is on and the
//!    LLM hasn't used any tools yet, push it to start. (One time only.)
//! 3. **Finalize** — default: let the loop end naturally.
//!
//! Removed (2026-07-09):
//! - **Patch blank bubble** — previously, if the LLM produced no visible text
//!   but a previous turn had visible text alongside tool calls, we'd reuse
//!   that text to fill the blank bubble. This is **fabricating LLM output**.
//! - **RetryReasoning inner-loop retry** — previously, when the LLM returned
//!   only reasoning_content, the inner loop retried up to 2 times. This was
//!   counterproductive: the reasoning-only message was added to context, so
//!   the LLM saw its own "answer" and returned reasoning-only again.
//!
//! Added (2026-07-09):
//! - **Post-tool nudge** — when the LLM stops after tool execution with only
//!   reasoning_content (no visible text, no tool calls), inject ONE system
//!   message asking it to produce visible text or continue working. This gives
//!   the LLM the chance to "decide" — summarize results or call more tools.
//!   Fires once only. If the LLM still doesn't produce visible text, the
//!   `local_agent_service` falls back to `reasoning_content` as the visible
//!   reply so the user always sees something.

use crate::python_alignment::{looks_like_codex_intermediate_ack, CODEX_CONTINUE_USER_MESSAGE};
use crate::ApiMode;
use hermix_core::Message;

/// Hard cap on Codex-style ack continuations.
pub const CODEX_MAX_ACK_CONTINUATIONS: u32 = 2;

/// Max post-tool nudges before respecting the LLM's stop signal.
///
/// When the LLM stops after tool execution with only reasoning_content
/// (no visible text, no tool calls), we inject a system nudge to push it
/// to produce visible output or continue working. Previously this fired
/// only once (`== 0` check), which was too aggressive: the LLM often
/// needs 2-3 attempts to recover, especially when:
///   - The `<think>` tag wasn't closed, routing all text to reasoning
///   - The LLM was confused by a tool error and needs to re-orient
///   - The LLM's first nudge response was still thinking-only
///
/// After `MAX_POST_TOOL_NUDGES` attempts, we respect the LLM's decision
/// and finalize. The `local_agent_service` falls back to
/// `reasoning_content` as the visible reply so the user always sees
/// something.
pub const MAX_POST_TOOL_NUDGES: u32 = 3;

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
    pub post_tool_nudge_count: u32,
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
    pub new_post_tool_nudge_count: u32,
}

/// Single authority for "what should the loop do when the LLM produces
/// no tool calls this turn".
///
/// Priority order:
/// 1. Codex ack continuation (protocol need)
/// 2. Post-tool nudge (push LLM to produce visible text after tools — one time only)
/// 3. Autonomous pre-tool nudge (push LLM to start working — one time only)
/// 4. Finalize (default — trust the LLM's stop signal)
pub fn decide_empty_response(
    input: EmptyResponseInput<'_>,
) -> EmptyResponseOutput {
    // 1. Codex ack continuation — protocol-level: the Codex Responses
    //    API sometimes emits a planning/ack reply that needs an explicit
    //    "continue" user message to proceed.
    if input.api_mode == ApiMode::CodexResponses
        && input.has_tool_schemas
        && input.codex_ack_continuations < CODEX_MAX_ACK_CONTINUATIONS
        && looks_like_codex_intermediate_ack(
            input.task_hint,
            input.assistant_msg.content.as_deref().unwrap_or(""),
            input.history_includes_tool,
        )
    {
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithUserMsg(
                CODEX_CONTINUE_USER_MESSAGE.to_string(),
            ),
            new_codex_ack_continuations: input.codex_ack_continuations + 1,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_post_tool_nudge_count: input.post_tool_nudge_count,
        };
    }

    // 2. Post-tool nudge — when the LLM stops after tool execution
    //    with only reasoning_content (no visible text, no tool calls),
    //    give it up to MAX_POST_TOOL_NUDGES more turns to produce
    //    visible text or call more tools. This lets the LLM "decide"
    //    whether to summarize results or continue working — exactly
    //    what the user expects after a tool executes.
    //
    //    Previously this fired only once (count == 0), which was too
    //    aggressive: the LLM often needs 2-3 attempts to recover,
    //    especially when the `<think>` tag wasn't closed (routing all
    //    text to reasoning_content) or the LLM was confused by a tool
    //    error. With only one nudge, the loop would finalize while
    //    the LLM was still trying to work — the "agent stops without
    //    completing the task" bug.
    //
    //    After MAX_POST_TOOL_NUDGES attempts, respect the LLM's
    //    decision and finalize. The `local_agent_service` falls back
    //    to `reasoning_content` as the visible reply.
    if input.history_includes_tool
        && input.has_tool_schemas
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.post_tool_nudge_count < MAX_POST_TOOL_NUDGES
        && !(input.assistant_has_visible_text)(input.assistant_msg)
    {
        let nudge = if input.post_tool_nudge_count == 0 {
            // First nudge: gentle reminder.
            "You just completed tool execution. Provide a visible summary of the results for the user, or continue with the next tool call if more work is needed. Do not stop without visible output."
                .to_string()
        } else {
            // Subsequent nudges: more forceful, diagnose the issue.
            // The LLM's previous response(s) had no visible text —
            // likely because all text went to reasoning_content
            // (e.g. an unclosed <think> tag). Tell the LLM exactly
            // what went wrong so it can fix it.
            format!(
                "Your last {} response(s) produced NO visible text for the user — \
                 only reasoning_content was generated. This usually happens when \
                 you open a <think> tag but don't close it with </think>, causing \
                 all text to be routed to the hidden reasoning channel. \
                 You MUST either:\n\
                 1. Call a tool to continue working, OR\n\
                 2. Produce visible text (outside any <think> block) summarizing \
                    the results so far.\n\
                 Do NOT produce another reasoning-only response.",
                input.post_tool_nudge_count
            )
        };
        tracing::info!(
            nudge_count = input.post_tool_nudge_count + 1,
            max = MAX_POST_TOOL_NUDGES,
            total_turns = input.total_turns,
            "post_tool_nudge: injecting system nudge to push LLM for visible output"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_post_tool_nudge_count: input.post_tool_nudge_count + 1,
        };
    }

    // 3. Autonomous pre-tool nudge — when autonomous mode is on and the
    //    LLM hasn't used any tools yet, push it to start working.
    //    Fires once only (count == 0). If the LLM still doesn't use tools
    //    after one nudge, respect its decision and finalize.
    //
    //    This does NOT fire after tools have run — the post-tool nudge
    //    above handles that case.
    //
    //    2026-07-15: Removed the `!(assistant_has_visible_text)` guard.
    //    Previously, if the LLM produced visible text (e.g. "I'll help you
    //    with that") but no tool calls, the loop would finalize immediately.
    //    The user saw a reply but the task wasn't actually started. Now the
    //    nudge fires even when visible text is present, pushing the LLM to
    //    use tools. The nudge message is updated to acknowledge the text
    //    and redirect to tool usage. After one nudge, the LLM's decision
    //    is respected — if it still doesn't use tools, the loop finalizes.
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
        let has_visible = (input.assistant_has_visible_text)(input.assistant_msg);
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
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count + 1,
            new_post_tool_nudge_count: input.post_tool_nudge_count,
        };
    }

    // 4. Default: trust the LLM's stop signal and finalize.
    //
    //    If the LLM chose to stop without visible text, we respect
    //    that. The thinking content is in the collapsible block.
    //    The `local_agent_service` will fall back to `reasoning_content`
    //    as the visible reply if `content` is empty.
    EmptyResponseOutput {
        action: EmptyResponseAction::Finalize,
        new_codex_ack_continuations: input.codex_ack_continuations,
        new_autonomous_nudge_count: input.autonomous_nudge_count,
        new_post_tool_nudge_count: input.post_tool_nudge_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    fn make_assistant_msg(content: Option<&str>, has_tool_calls: bool) -> Message {
        let mut m = Message::assistant(content.unwrap_or("").to_string());
        if has_tool_calls {
            m.tool_calls = Some(vec![hermix_core::ToolCall {
                id: "test-id".to_string(),
                function: hermix_core::FunctionCall {
                    name: "memory".to_string(),
                    arguments: "{}".to_string(),
                },
            }]);
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
            post_tool_nudge_count: 0,
            total_turns: 3,
            max_turns: 30,
            max_autonomous_nudges: 8,
            assistant_has_visible_text: visible_text_fn,
        }
    }

    #[test]
    fn default_is_finalize() {
        let m = make_assistant_msg(Some("ok done"), false);
        let out = decide_empty_response(base_input(&m));
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    #[test]
    fn codex_ack_continuation_fires_on_codex_api_with_planning_reply() {
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
            false,
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
            false,
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        input.codex_ack_continuations = CODEX_MAX_ACK_CONTINUATIONS;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    #[test]
    fn post_tool_nudge_fires_when_no_visible_text() {
        // When tools have run and the LLM stops with no visible text,
        // the post-tool nudge should fire (count == 0).
        let m = make_assistant_msg(None, false);
        let input = base_input(&m); // history_includes_tool=true, post_tool_nudge_count=0
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("visible summary"),
                    "post-tool nudge should ask for visible summary, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_post_tool_nudge_count, 1);
    }

    #[test]
    fn post_tool_nudge_does_not_fire_when_visible_text_present() {
        // When the LLM produced visible text after tool execution,
        // no nudge — respect the LLM's answer.
        let m = make_assistant_msg(Some("20个子目录（车辆），无文件。"), false);
        let input = base_input(&m); // history_includes_tool=true
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize when LLM gave visible text"
        );
    }

    #[test]
    fn post_tool_nudge_fires_up_to_max() {
        // Post-tool nudge should fire up to MAX_POST_TOOL_NUDGES times.
        // count=0 → fires (gentle), count=1 → fires (forceful),
        // count=2 → fires (forceful), count=3 → Finalize.
        let m = make_assistant_msg(None, false);

        // count=1: should still fire (forceful nudge)
        let mut input = base_input(&m);
        input.post_tool_nudge_count = 1;
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("NO visible text"),
                    "forceful nudge should diagnose the issue, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge at count=1, got {:?}", other),
        }
        assert_eq!(out.new_post_tool_nudge_count, 2);

        // count=2: should still fire (forceful nudge)
        let mut input = base_input(&m);
        input.post_tool_nudge_count = 2;
        let out = decide_empty_response(input);
        assert!(
            matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)),
            "should still nudge at count=2"
        );
        assert_eq!(out.new_post_tool_nudge_count, 3);

        // count=MAX: should finalize
        let mut input = base_input(&m);
        input.post_tool_nudge_count = MAX_POST_TOOL_NUDGES;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after MAX_POST_TOOL_NUDGES"
        );
    }

    #[test]
    fn autonomous_nudge_fires_when_no_tools_yet() {
        let m = make_assistant_msg(None, false);
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
        // 2026-07-15: The autonomous nudge should fire even when the LLM
        // produced visible text, as long as no tools have been used yet.
        // The LLM might say "I'll help you with that" but not actually
        // start working — the nudge pushes it to use tools.
        let m = make_assistant_msg(
            Some("好的，我来帮你完成这个任务。"),
            false,
        );
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
        // After tools have been used, the autonomous pre-tool nudge
        // should NOT fire — the post-tool nudge handles that case.
        let m = make_assistant_msg(
            Some("20个子目录（车辆），无文件。"),
            false,
        );
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        input.autonomous_nudge_count = 0;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "must finalize when LLM gave visible text after tools — respect the answer"
        );
        assert_eq!(out.new_autonomous_nudge_count, 0);
    }

    #[test]
    fn autonomous_nudge_fires_only_once() {
        // After one nudge (count=1), if the LLM still stops without tools,
        // respect its decision and finalize.
        let m = make_assistant_msg(None, false);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        input.autonomous_nudge_count = 1;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after one nudge — respect the LLM's decision"
        );
    }

    #[test]
    fn no_patch_blank_bubble_even_with_fallback() {
        // Removed PatchLastAssistant: even when there's a fallback
        // from a previous turn's visible text, we don't fabricate
        // output. Finalize instead.
        let m = make_assistant_msg(None, false);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize — never fabricate LLM output"
        );
    }

    #[test]
    fn priority_codex_over_autonomous() {
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
            false,
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
}
