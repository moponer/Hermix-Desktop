//! Mid-run `/steer` support (Python `AIAgent.steer` parity subset).
//!
//! Hermes keeps user guidance in a pending buffer and injects it into the
//! **last tool result** in the conversation tail, prefixed with a provenance
//! marker (`User guidance:`) so models don't confuse it with tool output.

use std::sync::Mutex;

use hermix_core::{Message, MessageRole};

use crate::context::ContextManager;

const USER_GUIDANCE_MARKER: &str = "User guidance:";

/// Normalize steer text for acceptance (mirrors Python tests).
pub fn normalize_accepted_steer_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Append a new steer fragment to an existing pending buffer (newline separated).
pub fn concat_pending(existing: Option<String>, next: String) -> String {
    match existing {
        None => next,
        Some(prev) if prev.is_empty() => next,
        Some(prev) => format!("{prev}\n{next}"),
    }
}

fn append_user_guidance_to_text(base: &str, guidance: &str) -> String {
    let guidance_block = format!("\n\n{USER_GUIDANCE_MARKER}\n{guidance}");
    format!("{base}{guidance_block}")
}

/// Apply pending steer guidance to the last `MessageRole::Tool` message in `messages`.
///
/// Returns `true` when a tool message was found and mutated.
pub fn apply_pending_steer_to_last_tool_message(messages: &mut [Message], guidance: &str) -> bool {
    let Some(idx) = messages.iter().rposition(|m| m.role == MessageRole::Tool) else {
        return false;
    };

    let msg = &mut messages[idx];
    let base = msg.content.clone().unwrap_or_default();
    msg.content = Some(append_user_guidance_to_text(&base, guidance));
    true
}

/// Drain the pending-steer buffer and return its current contents,
/// if any. Returns `None` when the buffer is empty or the lock is
/// poisoned.
pub fn drain_pending_steer(pending_steer: &Mutex<Option<String>>) -> Option<String> {
    pending_steer
        .lock()
        .ok()
        .and_then(|mut g| g.take())
}

/// Restash a previously-drained steer text back into the buffer.
/// Used when the call site didn't find a `Tool` message to inject
/// into (the steer would otherwise be silently dropped).
pub fn restash_pending_steer(pending_steer: &Mutex<Option<String>>, text: String) {
    if let Ok(mut guard) = pending_steer.lock() {
        *guard = Some(text);
    }
}

/// Pre-LLM-call drain: inject the pending steer text into the last
/// tool result if one exists, otherwise restash so the post-tool-call
/// path can try again on the next turn.
pub fn pre_api_apply_pending_steer(
    pending_steer: &Mutex<Option<String>>,
    ctx: &mut ContextManager,
) {
    let Some(text) = drain_pending_steer(pending_steer) else {
        return;
    };
    let msgs = ctx.get_messages_mut();
    if apply_pending_steer_to_last_tool_message(msgs, &text) {
        return;
    }
    restash_pending_steer(pending_steer, text);
}

/// Post-tool-call apply: same as `pre_api_apply_pending_steer` but
/// called after tool dispatch — the typical case is that the previous
/// `pre_api_apply_pending_steer` found no `Tool` message (because the
/// loop was mid-tool-call when steer arrived) and restashed; this
/// call lands the steer now that tool results are in the context.
pub fn post_tool_apply_pending_steer(
    pending_steer: &Mutex<Option<String>>,
    ctx: &mut ContextManager,
) {
    let Some(text) = drain_pending_steer(pending_steer) else {
        return;
    };
    let msgs = ctx.get_messages_mut();
    if apply_pending_steer_to_last_tool_message(msgs, &text) {
        return;
    }
    restash_pending_steer(pending_steer, text);
}

/// Queue mid-run user guidance (Python `AIAgent.steer`).
///
/// Returns `false` when the text is empty/whitespace-only.
pub fn steer(pending_steer: &Mutex<Option<String>>, text: &str) -> bool {
    let Some(next) = normalize_accepted_steer_text(text) else {
        return false;
    };
    if let Ok(mut guard) = pending_steer.lock() {
        *guard = Some(concat_pending(guard.take(), next));
        return true;
    }
    false
}

/// Clear any queued `/steer` guidance without applying it.
pub fn clear_pending_steer(pending_steer: &Mutex<Option<String>>) {
    if let Ok(mut guard) = pending_steer.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    #[test]
    fn accepts_trims_and_rejects_empty() {
        assert_eq!(
            normalize_accepted_steer_text("  hello \n").as_deref(),
            Some("hello")
        );
        assert_eq!(normalize_accepted_steer_text(""), None);
        assert_eq!(normalize_accepted_steer_text("  \n\t "), None);
    }

    #[test]
    fn concatenates_multiple_steers() {
        let mut cur: Option<String> = None;
        cur = Some(concat_pending(cur, "first".into()));
        cur = Some(concat_pending(cur, "second".into()));
        cur = Some(concat_pending(cur, "third".into()));
        assert_eq!(cur.as_deref(), Some("first\nsecond\nthird"));
    }

    #[test]
    fn appends_to_last_tool_result_only() {
        let mut msgs = vec![
            Message::user("u"),
            Message::tool_result("a", "A", Some("tool_a")),
            Message::tool_result("b", "B", Some("tool_b")),
        ];
        assert!(apply_pending_steer_to_last_tool_message(&mut msgs, "note"));
        assert_eq!(msgs[1].content.as_deref(), Some("A"));
        let b = msgs[2].content.as_deref().unwrap_or("");
        assert!(b.starts_with("B"));
        assert!(b.contains("User guidance:"));
        assert!(b.contains("note"));
    }
}
