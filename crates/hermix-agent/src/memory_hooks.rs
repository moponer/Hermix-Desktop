//! Pure helper functions used by `agent_loop`'s memory hooks.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `memory_write_event_from_tool_call` — parse `memory` tool call args
//!   into `(action, target, content)` for the router fan-out.
//! - `delegation_event_from_tool_result` — parse `delegate_task` result
//!   into `(task, sub_agent_id)` for the router fan-out.
//! - `build_session_summary` — short human-readable summary used by
//!   `memory_on_session_end` to seed a session-level `FocusConclusion`.

use hermix_core::{Message, ToolCall, ToolResult};
use serde_json::Value;

/// Parse a `memory` tool call into `(action, target, content)` for the
/// `MemoryRouter::on_memory_write` fan-out.
///
/// Returns `None` for non-`memory` tool calls or unparseable arguments.
pub fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
    if tc.function.name != "memory" {
        return None;
    }
    let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    if action != "add" && action != "replace" && action != "remove" {
        return None;
    }
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("memory")
        .to_string();
    let content = if action == "remove" {
        args.get("old_text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    } else {
        args.get("content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    Some((action, target, content))
}

/// Parse a `delegate_task` result into `(task, sub_agent_id)` for the
/// `MemoryRouter::on_delegation` fan-out.
///
/// Returns `None` for non-`delegate_task` tool calls, errored results, or
/// results missing the required `task` field.
pub fn delegation_event_from_tool_result(
    tc: &ToolCall,
    result: &ToolResult,
) -> Option<(String, String)> {
    if tc.function.name != "delegate_task" || result.is_error {
        return None;
    }
    let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();

    let sub_agent_id = serde_json::from_str::<Value>(&result.content)
        .ok()
        .and_then(|v| {
            v.get("sub_agent_id")
                .and_then(|id| id.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_default();

    Some((task, sub_agent_id))
}

/// Build a short human-readable summary of the session from the
/// last user turn + last assistant turn. Used by
/// `AgentLoop::memory_on_session_end` to write a session-level
/// `FocusConclusion` to the embedded SQLite (memory.db) so
/// cross-session recall works even when the session never used
/// `delegate_task`.
///
/// Returns `None` when the messages vector has no user content
/// (e.g. an aborted / empty session) — in that case writing a
/// meaningless empty row would only pollute recall.
pub fn build_session_summary(messages: &[Message]) -> Option<String> {
    use hermix_core::MessageRole;
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)?;
    let user_text = last_user.content.as_deref().unwrap_or("").trim();
    if user_text.is_empty() {
        return None;
    }
    let user_truncated = if user_text.chars().count() > 200 {
        format!(
            "{}…",
            hermix_core::text_utils::truncate_chars(user_text, 200)
        )
    } else {
        user_text.to_string()
    };
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .and_then(|m| m.content.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match last_assistant {
        Some(a) => {
            let a_truncated = if a.chars().count() > 400 {
                format!("{}…", hermix_core::text_utils::truncate_chars(a, 400))
            } else {
                a.to_string()
            };
            Some(format!("USER: {user_truncated}\nASSISTANT: {a_truncated}"))
        }
        None => Some(format!("USER: {user_truncated}")),
    }
}