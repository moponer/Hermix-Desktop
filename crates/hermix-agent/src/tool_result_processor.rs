//! Shared tool-result message construction.
//!
//! `push_tool_result` is the single authority for wrapping a tool-call
//! reply into `Message::tool_result` with the correct label. Previously
//! this pattern was copy-pasted 6 times across `run()` and `run_stream()`
//! in `run_loop.rs`.

use hermix_core::{Message, ToolCall, ToolResult};

use crate::context::ContextManager;

/// Look up the tool name for `tool_call_id` from `tool_calls`, then
/// push `Message::tool_result(id, content, tool_name)` into `ctx`.
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
    ctx.add_message(Message::tool_result(tool_call_id, content, tool_name));
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
