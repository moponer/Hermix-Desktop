//! Shared tool-result message construction with RTK-style terminal filtering.
//!
//! `push_tool_result` is the single authority for wrapping a tool-call
//! reply into `Message::tool_result` with the correct label. Previously
//! this pattern was copy-pasted 6 times across `run()` and `run_stream()`
//! in `run_loop.rs`.
//!
//! Terminal tool results are passed through `terminal_filter::detect_and_filter`
//! before being added to context, enabling command-aware compression of
//! common outputs (git status, pytest, cargo build, ls, etc.).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use hermix_core::{Message, ToolCall, ToolResult};

use crate::context::ContextManager;
use crate::terminal_filter;

/// Cumulative characters saved by RTK filtering across all tool calls
/// in this process. Exposed via `/api/status` for observability.
static FILTER_SAVED_CHARS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static FILTER_SAVED_CALLS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));

/// Report cumulative RTK savings since process start.
pub fn rtk_savings() -> (u64, u64) {
    (
        FILTER_SAVED_CHARS.load(Ordering::Relaxed),
        FILTER_SAVED_CALLS.load(Ordering::Relaxed),
    )
}

/// Extract the shell command string from a tool call's JSON arguments.
/// Returns `None` if the argument cannot be parsed or the command field is missing.
fn extract_command(tool_call_id: &str, tool_calls: &[ToolCall]) -> Option<String> {
    let tc = tool_calls.iter().find(|tc| tc.id == tool_call_id)?;
    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).ok()?;
    // Try "command" field (terminal), then "cmd" (execute_code), then fallback
    args.get("command")
        .or_else(|| args.get("cmd"))
        .or_else(|| args.get("script"))
        .and_then(|c| c.as_str())
        .map(String::from)
}

/// Apply RTK-style filter for terminal-like tool outputs.
fn apply_terminal_filter(content: &str, tool_name: Option<&str>, tool_calls: &[ToolCall], tool_call_id: &str) -> String {
    // Only filter terminal and code_execution tools
    match tool_name {
        Some("terminal" | "terminal_session" | "execute_code") => {}
        _ => return content.to_string(),
    }
    let command = match extract_command(tool_call_id, tool_calls) {
        Some(cmd) => cmd,
        None => return content.to_string(),
    };
    match terminal_filter::detect_and_filter(&command, content) {
        Some(filtered) => {
            let saved = content.len().saturating_sub(filtered.len());
            if saved > 0 {
                FILTER_SAVED_CHARS.fetch_add(saved as u64, Ordering::Relaxed);
                FILTER_SAVED_CALLS.fetch_add(1, Ordering::Relaxed);
            }
            filtered
        }
        None => content.to_string(),
    }
}

/// Push a `ToolResult` struct into context (batch path, after budget
/// pressure injection).
pub fn push_tool_result_from_struct(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    result: &ToolResult,
) {
    push_tool_result(ctx, tool_calls, &result.tool_call_id, &result.content);
}

/// Look up the tool name for `tool_call_id` from `tool_calls`, then
/// push `Message::tool_result(id, content, tool_name)` into `ctx`.
/// For terminal tools, applies RTK-style command-aware output filtering.
pub fn push_tool_result(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    tool_call_id: &str,
    content: &str,
) {
    let tool_name = tool_calls
        .iter()
        .find(|tc| tc.id == tool_call_id)
        .map(|tc| tc.function.name.as_str());

    let filtered = apply_terminal_filter(content, tool_name, tool_calls, tool_call_id);
    ctx.add_message(Message::tool_result(tool_call_id, &filtered, tool_name));
}
