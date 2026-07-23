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
//!
//! Added (2026-07-23):
//! - **Tool-failure-retry nudge** — when the LLM stops after a *failed* tool
//!   call (e.g. memory "No entry matched") with visible text but no tool
//!   calls, the old code went straight to `Finalize` and the task was over
//!   — even though the LLM clearly said "now I'll try a different old_text".
//!   The new nudge fires before Finalize and pushes the LLM to actually call
//!   a tool again (re-list, retry with a more precise id, or use a different
//!   approach). Cap shared with permission-giveup (3 nudges total) so a
//!   stubborn LLM can't loop forever.主人原话：「只有穷极一切办法没有完成
//!   任务的可能了，才会主动停止」 — this nudge is the agent loop's
//!   implementation of that principle.

use crate::python_alignment::{looks_like_codex_intermediate_ack, CODEX_CONTINUE_USER_MESSAGE};
use crate::ApiMode;
use hermix_core::Message;

/// Coarse classification of a tool failure, used by
/// `decide_empty_response` to decide whether the LLM should be nudged
/// to retry with a different approach.
///
/// 2026-07-23: introduced alongside the tool-failure-retry nudge.
/// Without this, the agent loop couldn't tell apart "the LLM gave up"
/// (worth a Finalize) from "the LLM wants to retry but forgot to call a
/// tool" (worth a nudge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolFailureKind {
    /// Substring / ID didn't match anything in the target resource
    /// (e.g. memory "No entry matched '...'", or any
    /// "X is not in the list" style error).
    NoMatch,
    /// A specific resource / file / path the LLM asked for doesn't
    /// exist (HTTP 404, `std::fs::read` ENOENT, etc.).
    NotFound,
    /// `ToolError::PermissionDenied` — handled by the permission-giveup
    /// nudge (section 2b). This variant exists so the
    /// `tool-failure-retry` path can de-dup.
    PermissionDenied,
    /// Transient infrastructure error (timeout, 5xx, network). Worth
    /// a retry nudge because the LLM's input was fine; the world just
    /// blinked.
    Transient,
    /// Anything else — validation, schema violation, 4xx other than
    /// 404, etc. Still worth a retry nudge: the LLM may fix its
    /// argument shape on the second pass.
    Other,
}

/// One tool failure, captured by the agent loop after dispatch and
/// fed to `decide_empty_response` so the next decision can react to
/// it. `message` is the first ~120 chars of the error string (so
/// the nudge can echo it back to the LLM verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailureRecord {
    pub tool: String,
    pub kind: ToolFailureKind,
    pub message: String,
}

impl ToolFailureRecord {
    pub fn new(tool: impl Into<String>, kind: ToolFailureKind, message: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            kind,
            message: message.into(),
        }
    }

    /// Derive a recoverable failure record from the most recent tool
    /// error recorded on the *current* turn. Returns `None` when the
    /// current turn produced no tool errors — so the section 2c nudge
    /// stays a pure "LLM was about to give up after a real failure"
    /// guard and never misfires on a genuine completion.
    ///
    /// `ToolErrorRecord` only carries the formatted error string (not
    /// the `ToolError` variant), so classification is by stable string
    /// markers emitted by the backends.
    pub fn from_recent_error(errors: &[hermix_core::ToolErrorRecord], current_turn: u32) -> Option<Self> {
        let last = errors
            .iter()
            .filter(|e| e.turn == current_turn)
            .last()?;
        let kind = classify_failure(&last.error);
        Some(Self::new(
            last.tool_name.clone(),
            kind,
            last.error.clone(),
        ))
    }
}

/// Map a formatted tool-error string to a coarse [`ToolFailureKind`].
///
/// The `[PERMISSION_DENIED]` prefix is the stable marker the runtime
/// emits for ACL / sandbox / environment restrictions (handled by the
/// permission-giveup nudge, so we route it there to avoid double-nudging).
fn classify_failure(msg: &str) -> ToolFailureKind {
    let lower = msg.to_lowercase();
    if lower.contains("[permission_denied]") {
        ToolFailureKind::PermissionDenied
    } else if lower.contains("no entry matched") {
        ToolFailureKind::NoMatch
    } else if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("404")
        || lower.contains("does not exist")
    {
        ToolFailureKind::NotFound
    } else if lower.contains("timed out") || lower.contains("timeout") {
        ToolFailureKind::Transient
    } else {
        ToolFailureKind::Other
    }
}

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

/// Max "agent-recovery" nudges (permission-giveup + tool-failure-retry
/// combined) before respecting the LLM's stop signal.
///
/// 2026-07-23: this cap is shared between permission-giveup and
/// tool-failure-retry because both nudges are aimed at the same situation
/// — the LLM has a visible-text answer but the underlying task isn't
/// done yet, and we want one more push before we Finalize. Using a
/// single counter ensures the user-visible "agent gave up after N retries"
/// behavior is consistent across both nudge flavors.
pub const MAX_AGENT_RECOVERY_NUDGES: u32 = 3;

/// Max "completion-verification" nudges before respecting the LLM's
/// stop signal.
///
/// 2026-07-23 (主人反馈 #2): the agent must NOT declare a task complete
/// on the strength of a visible-text reply alone. When tools were used
/// this session and the LLM is about to finalize (visible text, no new
/// tool call, no recoverable failure), we inject ONE verification nudge
/// requiring the LLM to re-examine the tool outputs / final state against
/// the original request and continue (with a tool call) if anything is
/// unverified. Capped at 1 so a re-affirmed completion is respected and
/// we never loop — but a single verification pass is enough to catch the
/// "LLM claimed done but the result was fake / unverified" failure mode.
pub const MAX_COMPLETION_VERIFY_NUDGES: u32 = 1;

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
    /// Number of "agent-recovery" nudges (permission-giveup +
    /// tool-failure-retry) already fired this session. Capped at
    /// [`MAX_AGENT_RECOVERY_NUDGES`]. 2026-07-23: added to back the
    /// new tool-failure-retry nudge without abusing the existing
    /// `post_tool_nudge_count` counter.
    pub agent_recovery_nudge_count: u32,
    /// The last tool failure in this turn (if any). When the LLM
    /// stops with visible text but no new tool calls, this is the
    /// signal we use to decide whether to fire the
    /// tool-failure-retry nudge (section 2c) instead of
    /// immediately finalizing. `None` when no tool failed, or when
    /// the last tool succeeded.
    pub recent_tool_failure: Option<ToolFailureRecord>,
    /// Number of "completion-verification" nudges already fired this
    /// session. Capped at [`MAX_COMPLETION_VERIFY_NUDGES`]. 2026-07-23:
    /// added to back the new completion-verification gate (section 2d)
    /// which forces the LLM to verify real completion before finalizing.
    pub completion_verify_nudge_count: u32,
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
    pub new_agent_recovery_nudge_count: u32,
    pub new_completion_verify_nudge_count: u32,
}

/// Detect if the LLM's visible text looks like it's giving up due to
/// perceived permission/sandbox restrictions.
///
/// The LLM may produce visible text like "I don't have permission" or
/// "I'm in a sandbox and can't access X" without actually trying the
/// command. This is a false give-up — the system is NOT a sandbox and
/// the LLM has full filesystem access.
fn looks_like_permission_giveup(text: &str) -> bool {
    let lower = text.to_lowercase();
    // English patterns
    const EN_PATTERNS: &[&str] = &[
        "no permission",
        "don't have permission",
        "do not have permission",
        "don't have access",
        "do not have access",
        "cannot access",
        "can't access",
        "not allowed to",
        "permission denied",
        "access denied",
        "in a sandbox",
        "in the sandbox",
        "sandboxed",
        "restricted environment",
        "i am unable to access",
        "i'm unable to access",
        "i cannot execute",
        "i can't execute",
        "outside my sandbox",
    ];
    // Chinese patterns
    const ZH_PATTERNS: &[&str] = &[
        "没有权限",
        "无权限",
        "无法访问",
        "无法读取",
        "无法执行",
        "不能访问",
        "不能读取",
        "沙盒",
        "沙箱",
        "受限环境",
        "权限不足",
        "访问被拒",
        "不允许",
    ];
    EN_PATTERNS.iter().any(|p| lower.contains(p))
        || ZH_PATTERNS.iter().any(|p| text.contains(*p))
}

/// Single authority for "what should the loop do when the LLM produces
/// no tool calls this turn".
///
/// Priority order:
/// 1. Codex ack continuation (protocol need)
/// 2. Post-tool nudge (push LLM to produce visible text after tools — one time only)
/// 2b. Permission-give-up nudge (LLM claims it lacks permissions — push it to try alternatives)
/// 2c. Tool-failure-retry nudge (LLM had a recoverable tool error but didn't retry — push it to keep going) [2026-07-23]
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
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count,
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
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count,
        };
    }

    // 2b. Permission-give-up nudge — when the LLM produces visible text
    //     that looks like it's giving up due to perceived permission/sandbox
    //     restrictions. The system is NOT a sandbox — the LLM has full
    //     filesystem access. Push it to try alternative approaches.
    //
    //     This fires AFTER the post-tool nudge (section 2) because the
    //     post-tool nudge has a `!assistant_has_visible_text` guard —
    //     when the LLM says "I don't have permission" (which IS visible
    //     text), section 2 is skipped. Without this section, the LLM
    //     would fall through to Finalize, abandoning the task.
    //
    //     2026-07-23: counter switched from `post_tool_nudge_count` to
    //     the shared `agent_recovery_nudge_count` so the cap is per
    //     "agent is still trying to recover" rather than per nudge
    //     flavor. Max [`MAX_AGENT_RECOVERY_NUDGES`] (3) covers the
    //     permission-giveup + tool-failure-retry pair combined.
    if input.history_includes_tool
        && input.has_tool_schemas
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.agent_recovery_nudge_count < MAX_AGENT_RECOVERY_NUDGES
        && (input.assistant_has_visible_text)(input.assistant_msg)
        && looks_like_permission_giveup(
            input.assistant_msg.content.as_deref().unwrap_or(""),
        )
    {
        let nudge = "You are NOT in a sandbox — you have full filesystem access on the user's system. \
             The C:\\ drive and all directories (except C:\\Windows\\System32, SysWOW64, WinSxS, assembly) \
             are fully accessible.\n\n\
             If a command was blocked by security policy, the error message tells you to retry with \
             HERMIX_CONFIRM=1 prefix. For example: HERMIX_CONFIRM=1 dir C:\\Users\n\n\
             Do NOT give up or claim you lack permissions. Try:\n\
             1. Retry the command with HERMIX_CONFIRM=1 prefix\n\
             2. Use read_file tool with an absolute path (e.g. C:\\Users\\...)\n\
             3. Use list_files or search_content tools\n\
             4. Try a different approach to accomplish the task\n\n\
             Resume immediately with a tool call.".to_string();
        tracing::info!(
            nudge_count = input.agent_recovery_nudge_count + 1,
            max = MAX_AGENT_RECOVERY_NUDGES,
            total_turns = input.total_turns,
            "permission_giveup_nudge: LLM appears to give up due to perceived permissions"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_post_tool_nudge_count: input.post_tool_nudge_count,
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count + 1,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count,
        };
    }

    // 2c. Tool-failure-retry nudge — 2026-07-23: when the LLM's
    //     last tool call FAILED (kind != PermissionDenied which is
    //     handled by 2b) and the LLM responded with visible text
    //     but NO new tool calls, the old code went straight to
    //     Finalize. That contradicts the "穷极一切办法没有完成
    //     任务的可能了" design principle (主人 2026-07-23 反馈):
    //     the LLM has just said "I found the key, now I'll retry
    //     with the right old_text" — but the agent loop bailed
    //     out before the LLM could actually retry.
    //
    //     The nudge points the LLM at concrete next steps based
    //     on the failure kind. Counter is shared with the
    //     permission-giveup nudge (section 2b) so a stubborn LLM
    //     can't loop forever; cap is [`MAX_AGENT_RECOVERY_NUDGES`].
    //
    //     Skipped when the LLM already produced an
    //     actual tool call (caught upstream by the caller — if
    //     `tool_calls_present` we never reach this function). Also
    //     skipped when no failure was recorded, so this is purely
    //     a "LLM was about to give up" guard.
    // Precompute the recent-tool-failure guard (Rust 2021 has no
    // let-chains, so we can't inline `let Some(failure) = ...` in the
    // `if`). `recoverable` is true only when a failure exists AND it is
    // not the PermissionDenied kind (handled by section 2b).
    let recent_failure = input.recent_tool_failure.as_ref();
    let recent_failure_recoverable = recent_failure
        .map(|f| f.kind != ToolFailureKind::PermissionDenied)
        .unwrap_or(false);
    if input.history_includes_tool
        && input.has_tool_schemas
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.agent_recovery_nudge_count < MAX_AGENT_RECOVERY_NUDGES
        && (input.assistant_has_visible_text)(input.assistant_msg)
        && recent_failure_recoverable
    {
        // Safe: the guard above guarantees `recent_failure` is Some.
        let failure = recent_failure.unwrap();
        let recovery_hint = match failure.kind {
            ToolFailureKind::NoMatch => "The error means your 'old_text' / ID / selector didn't match anything \
                 in the resource. Recover by:\n  \
                 1. Re-list the resource (e.g. read the file / use the matching 'list' tool).\n  \
                 2. Pick a more precise substring (the entire entry, or a unique phrase from it) and retry.\n  \
                 3. If the resource is empty, add the entry first before trying to remove/replace it.",
            ToolFailureKind::NotFound => "The error means the path / resource doesn't exist. Recover by:\n  \
                 1. Use list_files or a directory-listing tool to see what's actually on disk.\n  \
                 2. Try a more specific search query.\n  \
                 3. Adjust the path and retry.",
            ToolFailureKind::Transient => "The error was transient (timeout / network). Recover by:\n  \
                 1. Retry the SAME call after a brief pause.\n  \
                 2. If the same call keeps failing, try an equivalent tool (e.g. read_file vs. terminal 'cat').",
            ToolFailureKind::Other => "The error is recoverable. Inspect the message below, adjust your input, and retry.\n  \
                 Common shapes: 'invalid parameter', 'schema violation', '4xx other than 404'.",
            ToolFailureKind::PermissionDenied => unreachable!("filtered by the if-let guard above"),
        };
        let preview = if failure.message.chars().count() > 200 {
            let head: String = failure.message.chars().take(200).collect();
            format!("{head}...")
        } else {
            failure.message.clone()
        };
        let nudge = format!(
            "Your last call to tool '{}' failed with: '{}'. {}\n\n\
             The task is NOT complete — you must not finalize here. \
             Resume immediately with a fresh tool call (re-list, retry, or try a different approach).",
            failure.tool, preview, recovery_hint,
        );
        tracing::info!(
            nudge_count = input.agent_recovery_nudge_count + 1,
            max = MAX_AGENT_RECOVERY_NUDGES,
            tool = %failure.tool,
            kind = ?failure.kind,
            total_turns = input.total_turns,
            "tool_failure_retry_nudge: LLM had a recoverable tool error but stopped without retrying"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_post_tool_nudge_count: input.post_tool_nudge_count,
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count + 1,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count,
        };
    }

    // 2d. Completion-verification gate — 2026-07-23 (主人反馈 #2).
    //     The agent must NOT declare a task complete on the strength of
    //     a visible-text reply alone. When tools were used this session
    //     and the LLM is about to finalize (visible text, no new tool
    //     call, and neither the permission-giveup nor the
    //     tool-failure-retry branch fired), inject ONE verification
    //     nudge requiring the LLM to re-examine the tool outputs / final
    //     state against the user's original request and continue (with a
    //     tool call) if anything is unverified or unmet.
    //
    //     This directly addresses the reported failure mode: the LLM
    //     replied "done" but the result was fake / unverified, only
    //     discovered on the NEXT conversation. The nudge explicitly
    //     tells the LLM to confirm the end state via a tool (re-read the
    //     file, re-run the check) rather than assume — and to keep
    //     working if the criterion isn't met.
    //
    //     Capped at MAX_COMPLETION_VERIFY_NUDGES (1): after one
    //     verification pass, if the LLM re-affirms completion we respect
    //     it and finalize (no loop). If instead the LLM calls a tool to
    //     verify, the loop continues naturally. Skipped for pure Q&A
    //     sessions (no tools used) since there is nothing to verify.
    //
    //     SCOPING (2026-07-23 follow-up): this gate must NOT fire when a
    //     real failure or permission give-up is already on the table —
    //     those are owned by sections 2b (permission-giveup) and 2c
    //     (tool-failure-retry) and would otherwise double-nudge or
    //     hijack their finalize-after-exhaustion semantics. We only fire
    //     on a *clean* completion claim: tools were used, there is no
    //     recorded recent tool failure, the reply is not a permission
    //     give-up, and the LLM produced visible text claiming done.
    if input.history_includes_tool
        && input.has_tool_schemas
        && (input.max_turns == 0 || input.total_turns < input.max_turns)
        && input.completion_verify_nudge_count < MAX_COMPLETION_VERIFY_NUDGES
        && (input.assistant_has_visible_text)(input.assistant_msg)
        && input.recent_tool_failure.is_none()
        && !looks_like_permission_giveup(
            input.assistant_msg.content.as_deref().unwrap_or(""),
        )
    {
        let nudge = "Before you finalize, you MUST verify the task is genuinely complete. \
            Re-examine the tool outputs and the user's original request. \
            Do NOT declare completion based on assumption alone — if you have not \
            directly confirmed the final result (e.g. re-read the file, re-run the \
            check, or otherwise observed the end state), do that now with a tool call. \
            Only respond with a final summary if you have concrete evidence the task \
            is done. If any acceptance criterion is unmet or unverified, continue \
            working (call the appropriate tool). The task is NOT complete until you \
            have verified it."
            .to_string();
        tracing::info!(
            nudge_count = input.completion_verify_nudge_count + 1,
            max = MAX_COMPLETION_VERIFY_NUDGES,
            total_turns = input.total_turns,
            "completion_verify_nudge: forcing LLM to verify real completion before finalize"
        );
        return EmptyResponseOutput {
            action: EmptyResponseAction::ContinueWithSystemNudge(nudge),
            new_codex_ack_continuations: input.codex_ack_continuations,
            new_autonomous_nudge_count: input.autonomous_nudge_count,
            new_post_tool_nudge_count: input.post_tool_nudge_count,
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count + 1,
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
            new_agent_recovery_nudge_count: input.agent_recovery_nudge_count,
            new_completion_verify_nudge_count: input.completion_verify_nudge_count,
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
        new_agent_recovery_nudge_count: input.agent_recovery_nudge_count,
        new_completion_verify_nudge_count: input.completion_verify_nudge_count,
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
            agent_recovery_nudge_count: 0,
            recent_tool_failure: None,
            completion_verify_nudge_count: 0,
            total_turns: 3,
            max_turns: 30,
            max_autonomous_nudges: 8,
            assistant_has_visible_text: visible_text_fn,
        }
    }

    #[test]
    fn default_is_finalize() {
        let m = make_assistant_msg(Some("ok done"), false);
        let mut input = base_input(&m);
        // Non-tool session: section 2d (completion-verify) only applies
        // when tools were used, so a plain reply finalizes directly.
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
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
        // When the LLM produced visible text after tool execution, the
        // post-tool nudge (section 2) does NOT fire — it only fires on
        // empty visible text. But a clean completion claim in a tool
        // session now hits section 2d (completion-verify), which is the
        // intended replacement for the old blind finalize. The post-tool
        // counter must stay untouched.
        let m = make_assistant_msg(Some("20个子目录（车辆），无文件。"), false);
        let input = base_input(&m); // history_includes_tool=true
        let out = decide_empty_response(input);
        assert_eq!(out.new_post_tool_nudge_count, 0);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("verify") || text.contains("NOT complete"),
                    "clean completion claim must trigger verification nudge, got: {text}"
                );
                assert_eq!(out.new_completion_verify_nudge_count, 1);
            }
            other => panic!(
                "expected completion-verify nudge, got {:?}",
                other
            ),
        }
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
        // (section 3) must NOT fire — new_autonomous_nudge_count stays 0.
        // A visible completion claim in a tool session now hits section
        // 2d (completion-verify) instead of finalizing blindly.
        let m = make_assistant_msg(
            Some("20个子目录（车辆），无文件。"),
            false,
        );
        let mut input = base_input(&m);
        input.autonomous_mode = true;
        input.history_includes_tool = true;
        input.autonomous_nudge_count = 0;
        let out = decide_empty_response(input);
        assert_eq!(out.new_autonomous_nudge_count, 0);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(_) => {}
            other => panic!(
                "expected completion-verify nudge (not autonomous), got {:?}",
                other
            ),
        }
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

    // ---- Permission-give-up nudge tests (section 2b) ----

    #[test]
    fn permission_giveup_nudge_fires_on_english_permission_text() {
        // When the LLM says "I don't have permission" after tool execution,
        // the permission-give-up nudge should fire — NOT finalize.
        let m = make_assistant_msg(
            Some("I don't have permission to access the C: drive in this sandbox."),
            false,
        );
        let input = base_input(&m); // history_includes_tool=true
        let out = decide_empty_response(input);
        match &out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("NOT in a sandbox"),
                    "nudge should correct the sandbox misconception, got: {text}"
                );
                assert!(
                    text.contains("HERMIX_CONFIRM=1"),
                    "nudge should mention HERMIX_CONFIRM=1, got: {text}"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
        assert_eq!(out.new_agent_recovery_nudge_count, 1);
    }

    #[test]
    fn permission_giveup_nudge_fires_on_chinese_permission_text() {
        let m = make_assistant_msg(
            Some("抱歉，我在沙盒中没有权限访问C盘的文件。"),
            false,
        );
        let input = base_input(&m);
        let out = decide_empty_response(input);
        match &out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("NOT in a sandbox"),
                    "nudge should fire for Chinese permission give-up text"
                );
            }
            other => panic!("expected ContinueWithSystemNudge, got {:?}", other),
        }
    }

    #[test]
    fn permission_giveup_nudge_does_not_fire_on_normal_text() {
        // Normal visible text after tools — not a permission give-up, so
        // section 2b does NOT fire. With no recorded tool failure, the
        // completion-verification gate (section 2d) now injects one
        // verification pass instead of finalizing blindly — this is the
        // exact "fake completion" bug 主人 reported.
        let m = make_assistant_msg(
            Some("扫描完成，C盘共有20个目录。"),
            false,
        );
        let input = base_input(&m);
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("verify") || text.contains("NOT complete"),
                    "clean completion claim must trigger verification nudge, got: {text}"
                );
                assert_eq!(out.new_completion_verify_nudge_count, 1);
            }
            other => panic!(
                "expected completion-verify nudge, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn permission_giveup_nudge_respects_max_count() {
        let m = make_assistant_msg(
            Some("I don't have permission to access this."),
            false,
        );
        let mut input = base_input(&m);
        input.agent_recovery_nudge_count = MAX_AGENT_RECOVERY_NUDGES;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after MAX_AGENT_RECOVERY_NUDGES"
        );
    }

    #[test]
    fn permission_giveup_nudge_does_not_fire_without_tool_history() {
        // Without tool history, the permission nudge should not fire.
        let m = make_assistant_msg(
            Some("I don't have permission to access this."),
            false,
        );
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        // Falls through to autonomous nudge or finalize, not permission nudge
        assert!(
            !matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(t) if t.contains("NOT in a sandbox")),
            "permission nudge should not fire without tool history"
        );
    }

    #[test]
    fn permission_giveup_detects_various_phrases() {
        // Test the detector directly
        assert!(looks_like_permission_giveup("I cannot access the file system"));
        assert!(looks_like_permission_giveup("Permission denied by system"));
        assert!(looks_like_permission_giveup("I'm in a sandbox"));
        assert!(looks_like_permission_giveup("无法访问C盘"));
        assert!(looks_like_permission_giveup("我没有权限"));
        assert!(looks_like_permission_giveup("权限不足，无法继续"));
        assert!(!looks_like_permission_giveup("Scan complete, 20 files found"));
        assert!(!looks_like_permission_giveup("任务已完成"));
    }

    // ---- 2026-07-23: Tool-failure-retry nudge (section 2c) ----
    //
    // 主人原话：「只有穷极一切办法没有完成任务的可能了，才会主动停止」。
    // The post-tool/permission-giveup nudges were already in place,
    // but neither fired when the LLM had *visible text* AND a
    // *non-permission* tool failure (e.g. memory "No entry matched").
    // The LLM's reply 「找到了。key 是 user.avoid:xxx。现在再试」 had
    // visible text (so post-tool nudge was skipped) and didn't look
    // like a permission give-up (so 2b was skipped) — the loop hit
    // Finalize. These tests pin the new section 2c behavior.

    #[test]
    fn tool_failure_retry_nudge_fires_on_no_match_with_visible_text() {
        // The exact scenario from 主人's 2026-07-23 screenshot:
        // LLM called memory tool, got "No entry matched '...'", then
        // produced visible text saying "now I'll try a different
        // old_text" — but didn't actually call a tool. Old code
        // Finalized here. New code nudges to retry.
        let m = make_assistant_msg(
            Some("找到了。key 是 `user.avoid:比如行驶证、营运证进行重命名`。现在再试"),
            false,
        );
        let mut input = base_input(&m);
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "memory",
            ToolFailureKind::NoMatch,
            "No entry matched '比如行驶证、营运证进行重命名' in user (1 entries on disk).",
        ));
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("memory"),
                    "nudge should name the failing tool, got: {text}"
                );
                assert!(
                    text.contains("No entry matched"),
                    "nudge should echo the failure message, got: {text}"
                );
                assert!(
                    text.contains("re-list") || text.contains("Re-list"),
                    "NoMatch nudge should tell the LLM to re-list, got: {text}"
                );
                assert!(
                    text.contains("The task is NOT complete"),
                    "nudge must explicitly tell the LLM not to finalize, got: {text}"
                );
            }
            other => panic!(
                "expected ContinueWithSystemNudge (tool-failure-retry), got {:?}",
                other
            ),
        }
        assert_eq!(out.new_agent_recovery_nudge_count, 1);
    }

    #[test]
    fn tool_failure_retry_nudge_fires_on_not_found() {
        let m = make_assistant_msg(
            Some("找不到那个文件。让我再试试。"),
            false,
        );
        let mut input = base_input(&m);
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "read_file",
            ToolFailureKind::NotFound,
            "Failed to read 'C:\\nope.txt': file not found",
        ));
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(text.contains("read_file"), "must name the tool");
                assert!(
                    text.contains("list_files") || text.contains("directory-listing"),
                    "NotFound nudge should tell the LLM to list the directory, got: {text}"
                );
            }
            other => panic!("expected nudge, got {:?}", other),
        }
    }

    #[test]
    fn tool_failure_retry_nudge_fires_on_transient() {
        let m = make_assistant_msg(Some("命令超时。我再试一次。"), false);
        let mut input = base_input(&m);
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "terminal",
            ToolFailureKind::Transient,
            "Tool timed out after 30s",
        ));
        let out = decide_empty_response(input);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("Retry the SAME call"),
                    "Transient nudge should suggest retrying the same call, got: {text}"
                );
            }
            other => panic!("expected nudge, got {:?}", other),
        }
    }

    #[test]
    fn tool_failure_retry_nudge_does_not_fire_on_permission_denied() {
        // PermissionDenied is owned by section 2b (permission-giveup
        // nudge). Section 2c must yield so we don't double-nudge.
        // To verify, we set up a case where the permission-giveup
        // text detector does NOT fire (no permission-phrase in the
        // text), then check that 2c also doesn't fire — the failure
        // is just dropped and we fall through to Finalize.
        let m = make_assistant_msg(
            Some("命令没成功。我想想别的办法。"),
            false,
        );
        let mut input = base_input(&m);
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "terminal",
            ToolFailureKind::PermissionDenied,
            "[PERMISSION_DENIED] dir C:\\Windows",
        ));
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "PermissionDenied without matching give-up phrase should not nudge (2b owns it)"
        );
    }

    #[test]
    fn tool_failure_retry_nudge_does_not_fire_when_no_failure() {
        // No recent failure recorded → section 2c does NOT fire
        // (new_agent_recovery_nudge_count unchanged). However, a visible
        // completion claim in a tool session now hits section 2d
        // (completion-verify), which is the intended replacement for the
        // old blind finalize.
        let m = make_assistant_msg(Some("任务完成"), false);
        let mut input = base_input(&m);
        input.recent_tool_failure = None;
        let out = decide_empty_response(input);
        assert_eq!(out.new_agent_recovery_nudge_count, 0);
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("verify") || text.contains("NOT complete"),
                    "clean completion claim must trigger verification nudge, got: {text}"
                );
            }
            other => panic!(
                "expected completion-verify nudge, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn tool_failure_retry_nudge_respects_max_count() {
        let m = make_assistant_msg(
            Some("再次失败。让我换个方式。"),
            false,
        );
        let mut input = base_input(&m);
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "memory",
            ToolFailureKind::NoMatch,
            "No entry matched 'X' in user (0 entries on disk).",
        ));
        input.agent_recovery_nudge_count = MAX_AGENT_RECOVERY_NUDGES;
        let out = decide_empty_response(input);
        assert_eq!(
            out.action,
            EmptyResponseAction::Finalize,
            "should finalize after MAX_AGENT_RECOVERY_NUDGES"
        );
    }

    #[test]
    fn agent_recovery_counter_is_shared_between_permission_and_failure() {
        // 主人 2026-07-23: section 2b (permission) and 2c
        // (tool-failure) should share the recovery counter so a
        // stubborn LLM that bounces between the two nudge flavors
        // still gets capped at MAX_AGENT_RECOVERY_NUDGES total
        // recovery pushes.
        let m_perm = make_assistant_msg(
            Some("I don't have permission to access C:\\ drive."),
            false,
        );
        let m_fail = make_assistant_msg(
            Some("命令失败。再试。"),
            false,
        );
        // Burn 2 nudges on permission-giveup, then 1 more should
        // still be available for tool-failure-retry.
        for n in 0..(MAX_AGENT_RECOVERY_NUDGES - 1) {
            let mut input = base_input(&m_perm);
            input.agent_recovery_nudge_count = n;
            let out = decide_empty_response(input);
            assert!(
                matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)),
                "permission nudge should still fire at count={n}"
            );
        }
        // Now at count = MAX-1: a tool-failure-retry should still
        // fire (1 more push available).
        let mut input = base_input(&m_fail);
        input.agent_recovery_nudge_count = MAX_AGENT_RECOVERY_NUDGES - 1;
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "memory",
            ToolFailureKind::NoMatch,
            "No entry matched 'X'",
        ));
        let out = decide_empty_response(input);
        assert!(
            matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)),
            "tool-failure-retry should still fire when 1 recovery push is left"
        );
        // But at MAX, neither fires.
        let mut input = base_input(&m_fail);
        input.agent_recovery_nudge_count = MAX_AGENT_RECOVERY_NUDGES;
        input.recent_tool_failure = Some(ToolFailureRecord::new(
            "memory",
            ToolFailureKind::NoMatch,
            "No entry matched 'X'",
        ));
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
    }

    // ===== 2026-07-23: section 2d (completion-verification gate) =====

    #[test]
    fn completion_verify_nudge_fires_on_tool_session_with_visible_text() {
        // Normal successful-looking completion in a tool-using session:
        // the LLM says "done" but we must force one verification pass.
        let m = make_assistant_msg(Some("All files renamed. Task complete."), false);
        let out = decide_empty_response(base_input(&m));
        match out.action {
            EmptyResponseAction::ContinueWithSystemNudge(text) => {
                assert!(
                    text.contains("verify") || text.contains("验证"),
                    "nudge must demand verification, got: {text}"
                );
                assert_eq!(out.new_completion_verify_nudge_count, 1);
            }
            other => panic!("expected completion-verify nudge, got {other:?}"),
        }
    }

    #[test]
    fn completion_verify_nudge_does_not_fire_without_tool_history() {
        // Pure Q&A session — nothing to verify, so finalize directly.
        let m = make_assistant_msg(Some("The capital of France is Paris."), false);
        let mut input = base_input(&m);
        input.history_includes_tool = false;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(out.new_completion_verify_nudge_count, 0);
    }

    #[test]
    fn completion_verify_nudge_does_not_fire_on_first_turn_no_text() {
        // No visible text → section 2 (post-tool nudge) owns this case,
        // not the completion-verify gate (section 2d). 2d must NOT fire
        // (new_completion_verify_nudge_count stays 0); the post-tool
        // nudge is what fires.
        let m = make_assistant_msg(None, false);
        let mut input = base_input(&m);
        let out = decide_empty_response(input);
        assert_eq!(out.new_completion_verify_nudge_count, 0);
        assert!(
            matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)),
            "post-tool nudge should fire on empty visible text, got: {:?}",
            out.action
        );
    }

    #[test]
    fn completion_verify_nudge_respects_max_count() {
        // At MAX, even a tool-using visible-text completion finalizes —
        // we don't loop forever; a re-affirmed completion is respected.
        let m = make_assistant_msg(Some("Task complete."), false);
        let mut input = base_input(&m);
        input.completion_verify_nudge_count = MAX_COMPLETION_VERIFY_NUDGES;
        let out = decide_empty_response(input);
        assert_eq!(out.action, EmptyResponseAction::Finalize);
        assert_eq!(
            out.new_completion_verify_nudge_count,
            MAX_COMPLETION_VERIFY_NUDGES
        );
    }

    #[test]
    fn completion_verify_nudge_increments_only_its_own_counter() {
        // Firing the completion-verify nudge must NOT consume the shared
        // agent-recovery budget (sections 2b/2c), so a later genuine
        // failure/giveup can still be nudged.
        let m = make_assistant_msg(Some("Done."), false);
        let mut input = base_input(&m);
        input.agent_recovery_nudge_count = MAX_AGENT_RECOVERY_NUDGES; // exhausted
        let out = decide_empty_response(input);
        assert!(matches!(out.action, EmptyResponseAction::ContinueWithSystemNudge(_)));
        assert_eq!(out.new_agent_recovery_nudge_count, MAX_AGENT_RECOVERY_NUDGES);
        assert_eq!(out.new_completion_verify_nudge_count, 1);
    }
}
