//! Mid-run `/steer` support (Python `AIAgent.steer` parity subset).
//!
//! A steer is a **user turn**, not tool output. The old implementation
//! appended it to the last `Tool` message's content — that corrupted the
//! tool result the model was reading (a tool that returned `A` now "returned"
//! `A\n\nUser guidance:\n…`), broke result-equality expectations, and made
//! cache/trace comparisons useless. Codex injects steering as a separate
//! `user` input item; we do the same: drain the buffer and push one
//! standalone user message with a provenance header.

use std::sync::Mutex;

use hermix_core::{Message, MessageRole};

use crate::context::ContextManager;

/// Header on the injected user message. Kept short and unambiguous: it tells
/// the model this is an in-flight instruction, not a new task.
const USER_GUIDANCE_HEADER: &str =
    "[User guidance — mid-run instruction from the user. It refines the current task; it does not replace it.]";

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

/// Append the guidance as a standalone user message. Returns `true`
/// (there is always somewhere to put a user message — that is the point
/// of the change: steering can no longer be dropped for lack of a tool
/// result to piggyback on).
pub fn push_steer_message(ctx: &mut ContextManager, guidance: &str) -> bool {
    let guidance = guidance.trim();
    if guidance.is_empty() {
        return false;
    }
    // Protocol guard: an assistant turn that requested tools must be followed
    // by its `tool` results before any user message. Injecting in that window
    // produces a request providers reject outright ("tool_calls must be
    // followed by tool messages"). Callers restash and try again after the
    // tool results land.
    if let Some(last) = ctx.get_messages().last() {
        let pending_tool_calls = last.role == MessageRole::Assistant
            && last
                .tool_calls
                .as_ref()
                .map(|calls| !calls.is_empty())
                .unwrap_or(false);
        if pending_tool_calls {
            return false;
        }
    }
    ctx.add_message(Message::user(format!(
        "{USER_GUIDANCE_HEADER}\n{guidance}"
    )));
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
        // Append, never overwrite: a fresh `steer()` may have arrived between
        // drain and restash, and clobbering it would silently drop the user's
        // newest instruction.
        *guard = Some(concat_pending(guard.take(), text));
    }
}

/// Pre-LLM-call drain: land the pending steer as a user message.
pub fn pre_api_apply_pending_steer(
    pending_steer: &Mutex<Option<String>>,
    ctx: &mut ContextManager,
) {
    let Some(text) = drain_pending_steer(pending_steer) else {
        return;
    };
    if push_steer_message(ctx, &text) {
        ctx.recompute_total_tokens();
    } else {
        restash_pending_steer(pending_steer, text);
    }
}

/// Post-tool-call drain: same landing rule. Running both is safe — the
/// buffer is drained on the first call, so a steer is injected exactly once.
pub fn post_tool_apply_pending_steer(
    pending_steer: &Mutex<Option<String>>,
    ctx: &mut ContextManager,
) {
    let Some(text) = drain_pending_steer(pending_steer) else {
        return;
    };
    if push_steer_message(ctx, &text) {
        ctx.recompute_total_tokens();
    } else {
        restash_pending_steer(pending_steer, text);
    }
}

/// Queue mid-run user guidance (Python `AIAgent.steer`).
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
    fn steering_lands_as_its_own_user_message_and_never_touches_tool_output() {
        let mut ctx = ContextManager::new(2048);
        ctx.add_message(Message::user("u"));
        ctx.add_message(Message::tool_result("a", "A", Some("tool_a")));
        ctx.add_message(Message::tool_result("b", "B", Some("tool_b")));

        assert!(push_steer_message(&mut ctx, "  focus on tests  "));

        let msgs = ctx.get_messages();
        // Tool outputs are byte-identical to what the tools returned.
        assert_eq!(msgs[1].content.as_deref(), Some("A"));
        assert_eq!(msgs[2].content.as_deref(), Some("B"));
        // The steer is a new user turn carrying the provenance header.
        let last = msgs.last().expect("steer message pushed");
        assert_eq!(last.role, hermix_core::MessageRole::User);
        let text = last.content.as_deref().unwrap_or("");
        assert!(text.contains("mid-run instruction"));
        assert!(text.contains("focus on tests"));
        assert!(!text.contains("  focus")); // trimmed
    }

    #[test]
    fn steer_is_deferred_while_tool_calls_await_results() {
        // assistant(tool_calls) with no tool results yet → injecting a user
        // message here breaks the provider protocol, so the push is refused
        // (caller restashes; post-tool path lands it later).
        let mut ctx = ContextManager::new(2048);
        let mut assistant = Message::assistant("");
        assistant.tool_calls = Some(vec![hermix_core::ToolCall {
            id: "c1".into(),
            function: hermix_core::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        }]);
        ctx.add_message(assistant);
        assert!(!push_steer_message(&mut ctx, "wait"));

        // Once the tool result is in place, the same steer is accepted.
        ctx.add_message(Message::tool_result("c1", "ok", Some("read_file")));
        assert!(push_steer_message(&mut ctx, "wait"));
    }

    #[test]
    fn empty_steer_is_not_pushed() {
        let mut ctx = ContextManager::new(2048);
        let before = ctx.get_messages().len();
        assert!(!push_steer_message(&mut ctx, "   \n "));
        assert_eq!(ctx.get_messages().len(), before);
    }
}
