//! Pre-compression memory flush helpers.
//!
//! ## Why
//!!
//! When `ContextCompressor` is about to summarise or drop a section of
//! the conversation history, the agent only has one cheap chance to
//! preserve what matters: *before* the older turns are merged into a
//! handoff summary. Hermes Agent's `flush_memories` does this by
//! injecting a single system message asking the agent to call the
//! `memory` tool with anything worth saving.
//!
//! ## What this module provides
//!
//! - [`should_flush_memories`] – cheap threshold check (skip when the
//!   conversation is too short to be worth salvaging).
//! - [`build_flush_sentinel`] – a [`Message`] the caller can prepend to
//!   the LLM-bound messages so the agent sees the prompt and may
//!   respond with `memory` tool calls.
//! - [`strip_flush_sentinels`] – remove every sentinel from a
//!   messages vector, so they never end up in the DB or in the next
//!   compressed snapshot.
//!
//! ## Non-goals
//!
//! - The caller (typically the run-loop driver) decides whether to
//!   *actually* invoke the LLM and execute memory writes. This module
//!   only owns the message-shaping helpers.
//! - We never hard-fail on flush failures — `should_flush_memories`
//!   returns `false` rather than `Err`, so the surrounding compressor
//!   can fall back to its normal pipeline.
//!
//! ## Sentinel format
//!
//! Sentinels are recognised by a unique tag (`FLUSH_SENTINEL_MARKER`)
//! embedded in their content. Tags survive serde round-trips and DB
//! serialisation, so a sentinel injected by one process can be stripped
//! by another without losing track of which messages to remove.

use hermix_core::Message;
use hermix_core::MessageRole;

/// Marker embedded in the sentinel content so it's easy to recognise
/// after serde round-trips, DB persistence, or process boundaries.
pub const FLUSH_SENTINEL_MARKER: &str = "[FLUSH_SENTINEL::2026-07-15]";

/// Default minimum conversation length (in user/assistant turns) before
/// a flush is even attempted. Below this threshold the older turns are
/// likely too thin to be worth salvaging and the flush turn itself
/// would be a net loss.
pub const DEFAULT_FLUSH_MIN_TURNS: usize = 6;

/// System message text shown to the agent immediately before a context
/// compression. The agent is expected to respond with `memory` tool
/// calls preserving any facts the user might still need.
pub const FLUSH_SENTINEL_TEXT: &str = "[FLUSH_SENTINEL::2026-07-15] Context is about to be \
compressed to free space. If anything from the recent turns above is worth keeping (user \
preferences, project facts, decisions, blockers), call the `memory` tool now with the items \
you want to preserve. Avoid saving ephemeral state (tool output snippets, raw logs). After \
the memory calls land, compression will resume normally.";

/// Returns `true` when the conversation has enough back-and-forth to
/// justify spending an extra LLM round on memory flushing. Uses a pure
/// turn count — no LLM, no API cost.
pub fn should_flush_memories(messages: &[Message], min_turns: usize) -> bool {
    if min_turns == 0 {
        return false;
    }
    // Count assistant + user turns as "turns" — system / tool messages
    // don't carry decision-grade content and shouldn't gate the flush.
    let turns = messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::User | MessageRole::Assistant))
        .count();
    turns >= min_turns
}

/// Build the system-role sentinel that asks the agent to flush memory
/// before compression runs. The marker tag is prepended so the message
/// can be detected and stripped later.
pub fn build_flush_sentinel() -> Message {
    Message {
        role: MessageRole::System,
        content: Some(format!("{FLUSH_SENTINEL_TEXT}\nmarker={FLUSH_SENTINEL_MARKER}")),
        content_parts: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
        execution_timeline: None,
        unified_steps: None,
        ephemeral: true,
        automation_meta: None,
    }
}

/// Prepend a flush sentinel to the message list — only if the
/// threshold check passes. Returns the unchanged list when the
/// conversation is too short or empty, so callers can chain this
/// without branching on the threshold.
pub fn maybe_inject_flush_sentinel(messages: Vec<Message>, min_turns: usize) -> Vec<Message> {
    if !should_flush_memories(&messages, min_turns) {
        return messages;
    }
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(build_flush_sentinel());
    out.extend(messages);
    out
}

/// Strip every flush sentinel from the messages list. Safe to call on
/// lists that don't contain any — they're returned unchanged.
pub fn strip_flush_sentinels(mut messages: Vec<Message>) -> Vec<Message> {
    messages.retain(|m| !is_flush_sentinel(m));
    messages
}

/// Recognise a sentinel message by inspecting its content for the
/// marker tag. We match on substring because the marker may be
/// embedded alongside the rest of the text after a serde round-trip.
pub fn is_flush_sentinel(message: &Message) -> bool {
    if !matches!(message.role, MessageRole::System) {
        return false;
    }
    match message.content.as_deref() {
        Some(content) => content.contains(FLUSH_SENTINEL_MARKER),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;
    use hermix_core::MessageRole;

    fn user_msg(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(text.to_string()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(text.to_string()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    fn sys_msg(text: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: Some(text.to_string()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    #[test]
    fn should_flush_returns_false_below_threshold() {
        let msgs = vec![user_msg("hi"), assistant_msg("hello"), user_msg("ok")];
        assert!(!should_flush_memories(&msgs, 6));
    }

    #[test]
    fn should_flush_returns_true_at_threshold() {
        // 6 turns total — meets the default
        let msgs = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            user_msg("u2"),
            assistant_msg("a2"),
            user_msg("u3"),
            assistant_msg("a3"),
        ];
        assert!(should_flush_memories(&msgs, 6));
    }

    #[test]
    fn should_flush_ignores_tool_and_system_messages_for_threshold() {
        let mut msgs = vec![user_msg("u1"), assistant_msg("a1")];
        // tool + system messages shouldn't push us past the threshold
        msgs.push(sys_msg("[CONTEXT COMPACTION] ..."));
        msgs.push(sys_msg("system note"));
        msgs.push(sys_msg("another system"));
        msgs.push(user_msg("u2"));
        msgs.push(assistant_msg("a2"));
        assert!(!should_flush_memories(&msgs, 6));
    }

    #[test]
    fn should_flush_disabled_when_min_turns_is_zero() {
        let msgs = vec![user_msg("hi")];
        assert!(!should_flush_memories(&msgs, 0));
    }

    #[test]
    fn build_flush_sentinel_is_recognisable() {
        let s = build_flush_sentinel();
        assert_eq!(s.role, MessageRole::System);
        let content = s.content.as_deref().unwrap_or("");
        assert!(content.contains(FLUSH_SENTINEL_MARKER));
        assert!(is_flush_sentinel(&s));
    }

    #[test]
    fn is_flush_sentinel_rejects_non_sentinel_system_messages() {
        let s = sys_msg("just a normal system message about policy");
        assert!(!is_flush_sentinel(&s));
    }

    #[test]
    fn maybe_inject_skips_when_below_threshold() {
        let msgs = vec![user_msg("hi"), assistant_msg("hello")];
        let out = maybe_inject_flush_sentinel(msgs.clone(), 6);
        assert_eq!(out.len(), msgs.len());
    }

    #[test]
    fn maybe_inject_prepends_when_at_threshold() {
        let msgs = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            user_msg("u2"),
            assistant_msg("a2"),
            user_msg("u3"),
            assistant_msg("a3"),
        ];
        let out = maybe_inject_flush_sentinel(msgs, 6);
        assert_eq!(out.len(), 7);
        assert!(is_flush_sentinel(&out[0]));
        // The original messages follow the sentinel in original order
        assert_eq!(out[1].content.as_deref(), Some("u1"));
    }

    #[test]
    fn strip_flush_sentinels_removes_only_sentinels() {
        let mut msgs = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            sys_msg("not a sentinel"),
            build_flush_sentinel(),
            user_msg("u2"),
            assistant_msg("a2"),
        ];
        let n_before = msgs.len();
        msgs = strip_flush_sentinels(msgs);
        assert_eq!(msgs.len(), n_before - 1);
        // Sanity: every remaining message is non-sentinel
        for m in &msgs {
            assert!(!is_flush_sentinel(m));
        }
    }

    #[test]
    fn strip_flush_sentinels_idempotent_on_clean_lists() {
        let msgs = vec![user_msg("u1"), assistant_msg("a1")];
        let out = strip_flush_sentinels(msgs.clone());
        assert_eq!(out.len(), msgs.len());
    }
}
