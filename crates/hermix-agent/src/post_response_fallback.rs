//! Protocol fallback single point (C2 refactor).
//!
//! ## Why this exists
//!
//! Before C2, the "no tool calls" branch in both `run_blocking` and `run`
//! open-coded four overlapping decisions inline:
//!
//! 1. **Codex ack continuation** — when running on the Codex Responses API
//!    and the assistant produced a planning/ack reply, inject a
//!    `[System: Continue now …]` user message and re-ask the LLM.
//! 2. **Autonomous-mode nudge** — when `autonomous_mode = true` and the LLM
//!    "stopped" mid-task without using a tool, push it back into action with
//!    a system message.
//! 3. **Patch last assistant content** — when the current turn's assistant
//!    message has no visible text after stripping think blocks, replace it
//!    with the most recent visible text the model produced alongside tool
//!    calls (so the user does not see a blank bubble).
//! 4. **Finalize** — hand back control to the loop, which will run the
//!    final memory sync and return the result.
//!
//! Those four decisions were copy-pasted across `run_blocking` and `run` —
//! see the `codex_ack_continuations`/`autonomous_nudge_count` blocks at
//! `agent_loop.rs:5347-5470` and `agent_loop.rs:6319-6470`. They are the
//! authority for whether the loop will emit a user-visible message when
//! the LLM produces no tool calls, so any divergence between the two
//! paths was a silent UX bug (different code, different fallback rules).
//!
//! `decide_post_response_fallback` is the **single authority** for that
//! decision. Both call sites now reduce to:
//!
//! ```ignore
//! let outcome = decide_post_response_fallback(input);
//! match outcome.action { /* continue / patch / finalize */ }
//! ```
//!
//! Unit tests in this file cover each priority branch in isolation so
//! the rules cannot drift.

use crate::python_alignment::{looks_like_codex_intermediate_ack, CODEX_CONTINUE_USER_MESSAGE};
use crate::ApiMode;
use hermix_core::Message;

/// Hard cap on Codex-style ack continuations. The original inline code
/// used `< 2`; preserved here so a future bump is one obvious place to
/// change. (Both call sites had this magic number duplicated.)
pub const CODEX_MAX_ACK_CONTINUATIONS: u32 = 2;

/// What should the agent loop do when the LLM responded with no tool
/// calls? This enum is the entire fallback surface — there is no
/// "default" branch at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostResponseFallback {
    /// Inject a user message (e.g. Codex continue) and start the next
    /// turn immediately.
    ContinueWithUserMsg(String),
    /// Inject a system nudge (autonomous mode) and start the next turn.
    ContinueWithSystemNudge(String),
    /// Replace the just-added assistant message's `content` with the
    /// supplied fallback text (used when the LLM emitted no visible
    /// text but previously emitted text alongside tool calls).
    PatchLastAssistant { content: String },
    /// Hand control back to the loop, which will run the final memory
    /// sync and return the result.
    Finalize,
}

/// All inputs the fallback decision needs. The function takes everything
/// by value/reference so the call site does not need to know which
/// internal state the decision touches.
#[derive(Debug)]
pub struct PostResponseFallbackInput<'a> {
    /// The assistant message returned by the LLM this turn.
    pub assistant_msg: &'a Message,
    /// The API mode in use (CodexResponses needs ack continuation,
    /// other modes skip it).
    pub api_mode: ApiMode,
    /// Whether the agent has any tool schemas at all (a model with no
    /// tools cannot use a "use a tool" nudge).
    pub has_tool_schemas: bool,
    /// Whether the conversation history contains any `MessageRole::Tool`
    /// entry (autonomous mode only nudges after at least one tool ran).
    pub history_includes_tool: bool,
    /// The original user task (used by the Codex ack detector).
    pub task_hint: &'a str,
    /// Current Codex ack continuation count.
    pub codex_ack_continuations: u32,
    /// Whether autonomous mode is on (autonomous-mode nudge is opt-in).
    pub autonomous_mode: bool,
    /// Current autonomous-mode nudge count.
    pub autonomous_nudge_count: u32,
    /// Current turn index (1-based).
    pub total_turns: u32,
    /// Maximum allowed turns for this run.
    pub max_turns: u32,
    /// Hard cap for the autonomous-mode nudge (typically 8).
    pub max_autonomous_nudges: u32,
    /// Visible text saved from the most recent assistant turn that
    /// emitted text alongside tool calls. Consumed (taken) by the
    /// decision so the caller does not need to clear it.
    pub last_content_with_tools: Option<String>,
    /// Helper: is there any visible text in the assistant message
    /// after stripping think blocks? Provided as a function pointer so
    /// the new module does not need to depend on the agent loop's
    /// private helper.
    pub assistant_has_visible_text: fn(&Message) -> bool,
}

/// The decision, plus the counter increments the caller should apply
/// before re-entering the loop body.
#[derive(Debug)]
pub struct PostResponseFallbackOutput {
    pub action: PostResponseFallback,
    pub new_codex_ack_continuations: u32,
    pub new_autonomous_nudge_count: u32,
}

/// Single authority for "what should the loop do when the LLM produces
/// no tool calls this turn". Priority order matches the original inline
/// code (Codex ack first, autonomous nudge second, fallback content
/// third, finalize last).
pub fn decide_post_response_fallback(
    input: PostResponseFallbackInput<'_>,
) -> PostResponseFallbackOutput {
    // 1. Codex ack continuation — only relevant on the Codex Responses
    //    API when the model emitted a planning/ack reply.
    if input.api_mode == ApiMode::CodexResponses
        && input.has_tool_schemas
        && input.codex_ack_continuations < CODEX_MAX_ACK_CONTINUATIONS
        && looks_like_codex_intermediate_ack(
            input.task_hint,
            input.assistant_msg.content.as_deref().unwrap_or(""),
            input.history_includes_tool,
        )
    {
        return PostResponseFallbackOutput {
            action: PostResponseFallback::ContinueWithUserMsg(
                CODEX_CONTINUE_USER_MESSAGE.to_string(),
            ),
            new_codex_ack_continuations: input.codex_ack_continuations + 1,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
        };
    }

    // 2. Autonomous-mode nudge — re-drive the LLM if it stopped mid-task.
    if input.autonomous_mode
        && input.has_tool_schemas
        && input.history_includes_tool
        && input.total_turns < input.max_turns
        && input.autonomous_nudge_count < input.max_autonomous_nudges
    {
        let nudge = format!(
            "You have not yet completed the user's task. \
             Tools are available and more turns remain ({} of {} used). \
             Do NOT stop here — continue working. \
             If you need information, use a tool to obtain it rather than asking the user. \
             Resume immediately with the next tool call.",
            input.total_turns, input.max_turns
        );
        return PostResponseFallbackOutput {
            action: PostResponseFallback::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count + 1,
        };
    }

    // 3. Patch the just-added assistant message with the most recent
    //    visible text, but only if the current message has no visible
    //    text of its own.
    if !(input.assistant_has_visible_text)(input.assistant_msg) {
        if let Some(fallback) = input.last_content_with_tools {
            return PostResponseFallbackOutput {
                action: PostResponseFallback::PatchLastAssistant { content: fallback },
                new_codex_ack_continuations: input.codex_ack_continuations,
                new_autonomous_nudge_count: input.autonomous_nudge_count,
            };
        }
    }

    // 4. Default: let the loop finalize.
    PostResponseFallbackOutput {
        action: PostResponseFallback::Finalize,
        new_codex_ack_continuations: input.codex_ack_continuations,
        new_autonomous_nudge_count: input.autonomous_nudge_count,
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

    // visible_text_fn used by tests: returns true if the message has
    // visible (non-think) text.
    fn visible_text_fn(m: &Message) -> bool {
        let Some(c) = m.content.as_deref() else {
            return false;
        };
        !c.trim().is_empty()
    }

    fn base_input<'a>(assistant: &'a Message) -> PostResponseFallbackInput<'a> {
        PostResponseFallbackInput {
            assistant_msg: assistant,
            api_mode: ApiMode::ChatCompletions,
            has_tool_schemas: true,
            history_includes_tool: true,
            task_hint: "do the task",
            codex_ack_continuations: 0,
            autonomous_mode: false,
            autonomous_nudge_count: 0,
            total_turns: 3,
            max_turns: 30,
            max_autonomous_nudges: 8,
            last_content_with_tools: None,
            assistant_has_visible_text: visible_text_fn,
        }
    }

    #[test]
    fn default_is_finalize() {
        let m = make_assistant_msg(Some("ok done"), false);
        let out = decide_post_response_fallback(base_input(&m));
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn codex_ack_continuation_fires_on_codex_api_with_planning_reply() {
        // codex ack detector requires: history_includes_tool=false +
        // assistant mentions "let me"/"i will" + action marker + workspace
        // context. Easiest: history_includes_tool=false (no tool yet) +
        // a reply that hits "let me" + "look into"/"read" + a task hint
        // that mentions a directory.
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
            false,
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        let out = decide_post_response_fallback(input);
        match out.action {
            PostResponseFallback::ContinueWithUserMsg(text) => {
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
        // Hits the cap → falls through to autonomous/finalize. With
        // autonomous_mode=false, history_includes_tool=true (base) but
        // no fallback set, expect Finalize.
        let out = decide_post_response_fallback(input);
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn autonomous_nudge_fires_when_stopped_mid_task() {
        let m = make_assistant_msg(Some("ok done"), false);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        // history_includes_tool=true (set in base_input)
        let out = decide_post_response_fallback(input);
        match out.action {
            PostResponseFallback::ContinueWithSystemNudge(text) => {
                assert!(text.contains("3 of 30"));
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_autonomous_nudge_count, 1);
    }

    #[test]
    fn autonomous_nudge_caps_at_max() {
        let m = make_assistant_msg(Some("ok done"), false);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.autonomous_nudge_count = input.max_autonomous_nudges;
        let out = decide_post_response_fallback(input);
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn autonomous_nudge_does_not_fire_when_no_history_tool() {
        let m = make_assistant_msg(Some("ok done"), false);
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = false;
        // No fallback content set either → Finalize.
        let out = decide_post_response_fallback(input);
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn patch_last_assistant_when_visible_text_empty_and_fallback_set() {
        let m = make_assistant_msg(None, false);
        let mut input = base_input(&m);
        input.last_content_with_tools = Some("previous text".to_string());
        let out = decide_post_response_fallback(input);
        assert_eq!(
            out.action,
            PostResponseFallback::PatchLastAssistant {
                content: "previous text".to_string()
            }
        );
    }

    #[test]
    fn patch_skipped_when_current_assistant_has_visible_text() {
        let m = make_assistant_msg(Some("ok done"), false);
        let mut input = base_input(&m);
        input.last_content_with_tools = Some("previous text".to_string());
        // Current message has visible text → no patch, finalize.
        let out = decide_post_response_fallback(input);
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn patch_skipped_when_no_fallback_set() {
        let m = make_assistant_msg(None, false);
        let mut input = base_input(&m);
        input.last_content_with_tools = None;
        let out = decide_post_response_fallback(input);
        assert_eq!(out.action, PostResponseFallback::Finalize);
    }

    #[test]
    fn priority_codex_over_autonomous() {
        // On CodexResponses with autonomous mode and a planning reply,
        // Codex ack should win (matches original inline order).
        let m = make_assistant_msg(
            Some("Let me look into the project directory to find what we need."),
            false,
        );
        let mut input = base_input(&m);
        input.api_mode = ApiMode::CodexResponses;
        input.history_includes_tool = false;
        input.task_hint = "scan the directory and list files";
        input.autonomous_mode = true;
        let out = decide_post_response_fallback(input);
        assert!(matches!(
            out.action,
            PostResponseFallback::ContinueWithUserMsg(_)
        ));
    }
}
