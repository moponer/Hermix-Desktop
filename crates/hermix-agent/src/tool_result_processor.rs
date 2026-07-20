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

/// 2026-07-18 (A+C product fix): when a tool call was **denied** (not
/// merely failed on execution) — user拒绝 / 超时 / 连接中断 — inject a
/// structured `system` note right after the `tool_result` so the LLM's
/// next turn is forced to (1) tell the user the call was blocked, (2)
/// explain why the tool was needed, and (3) ask how to proceed instead
/// of silently retrying or going quiet. Policy denials
/// (`denial_kind == "policy"`) are intentionally skipped — the LLM
/// already knows the policy gate fired from the `reason` string and a
/// second note would be noise.
///
/// No-op for normal results (`denial_kind == None`) and execution
/// errors (those are real failures, not refusals).
pub fn inject_denial_note_if_needed(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    result: &ToolResult,
) {
    let Some(kind_str) = result.denial_kind.as_deref() else {
        return;
    };
    let kind = crate::tool_dispatch::DenialKind::from_str(kind_str);
    // Policy denials (Tirith/hardline) are silent-by-design: the LLM
    // already knows the gate fired from the `reason` string, and a
    // second note would be noise.
    if !kind.requires_user_notice() {
        return;
    }
    let tool_name = tool_calls
        .iter()
        .find(|tc| tc.id == result.tool_call_id)
        .map(|tc| tc.function.name.as_str())
        .unwrap_or("<unknown>");

    let kind_label = match kind {
        crate::tool_dispatch::DenialKind::UserDenied => "用户明确拒绝了该工具调用",
        crate::tool_dispatch::DenialKind::TimedOut => "确认超时（你在限定时间内没有做出决定）",
        crate::tool_dispatch::DenialKind::ChannelClosed => {
            "连接中断（WS 断开 / 服务端重启 / 会话重载），请求丢失"
        }
        crate::tool_dispatch::DenialKind::Policy => "策略拦截", // unreachable after the guard above
    };

    let note = format!(
        "[SYSTEM — 工具调用被拒绝]\n\
         工具 `{tool_name}` 的调用未能执行。拒绝原因：{kind_label}。\n\
         你的【下一段回复必须】做到以下三点，不得遗漏：\n\
         1. 用中文明确告诉用户：刚才这个 `{tool_name}` 工具调用被拒绝，没有执行。\n\
         2. 简要说明你原本打算用这个工具做什么、为什么需要它。\n\
         3. 主动询问用户下一步：是否换用替代工具、调整请求描述，还是结束当前任务。\n\
         【严禁】在用户未授权的情况下，自作主张地用其它工具重试同样的操作——用户的拒绝决定必须被尊重。",
    );
    ctx.add_message(Message::system(note));
}

/// 2026-07-20 (sandbox-permission fix P3): when a tool failed with a
/// `PERMISSION_DENIED` error (filesystem ACL / sandbox virtualization /
/// missing admin / missing Windows OCR language pack / no interactive desktop
/// session, tagged `[PERMISSION_DENIED]` by `ToolError::from_message`), inject
/// a structured `system` note right after the `tool_result` so the LLM's next
/// turn is forced to (1) tell the user the action was blocked by the runtime,
/// (2) explain what access it needed, and (3) ask the user to grant that
/// access in the runtime — instead of silently retrying, treating virtualized
/// data as real, or fabricating a generic "Windows cleanup checklist" to paper
/// over the failure.
///
/// This enforces the boss philosophy: Hermix runs on a real Windows machine;
/// we never probe for a sandbox and never downgrade to a generic checklist — we
/// surface the restriction and hand the "request permission" action back to the
/// user. No-op for normal results and non-permission errors.
pub fn inject_permission_note_if_needed(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    result: &ToolResult,
) {
    if !result.is_error {
        return;
    }
    if !result.content.contains("[PERMISSION_DENIED]") {
        return;
    }
    let tool_name = tool_calls
        .iter()
        .find(|tc| tc.id == result.tool_call_id)
        .map(|tc| tc.function.name.as_str())
        .unwrap_or("<unknown>");

    let note = format!(
        "[SYSTEM — 运行环境权限/依赖受限]\n\
         工具 `{tool_name}` 的执行被运行环境的权限或依赖限制拦截（可能是：沙盒文件系统虚拟化、ACL 权限不足、缺少管理员权限、缺少 Windows OCR 语言包、或无交互桌面会话）。\n\
         你的【下一段回复必须】做到以下四点，不得遗漏：\n\
         1. 用中文明确告诉用户：刚才 `{tool_name}` 因运行环境限制【没有成功执行】，不要假装成功。\n\
         2. 说明你原本想做什么、为什么受限（例如：需要写系统目录的权限 / 需要 OCR 语言包 / 需要交互桌面会话）。\n\
         3. 直接请用户在运行环境（沙盒）放开对应权限、或以管理员身份运行 Hermix 后重试；若是缺少语言包或桌面会话，说明需要安装/开启什么。\n\
         4. 不要编造\"通用清单\"或把虚拟化视图当成真机数据糊弄用户。\n\
         【严禁】在权限未放开前反复重试同一操作。",
    );
    ctx.add_message(Message::system(note));
}

/// v34 (2026-07-09): push a `tool_result` message that exists **only**
/// in the in-memory context — it is never persisted to the DB.
/// Used by the replay path where the tool wasn't actually re-executed;
/// the message is needed to satisfy the provider API's `tool_call_id`
/// pairing requirement, but the original execution's `tool_result` is
/// already persisted. Creating a second non-ephemeral row would produce
/// duplicate cards on the frontend after a session reload.
///
/// v35 (2026-07-09): the content is prefixed with a `[CACHED]` marker
/// so the LLM can clearly see this is a replay of a previous call —
/// not a new execution. Without this marker, the LLM has no way to
/// distinguish a fresh tool result from a cached one, and may re-issue
/// the same call in the next turn because it thinks the tool hasn't
/// been called yet.
pub fn push_ephemeral_tool_result(
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
    // Prefix with [CACHED] so the LLM knows this is a replay, not a
    // fresh execution. This is the root-cause fix for the "LLM
    // re-issues the same tool call" bug — without this marker, the
    // LLM sees identical tool_result content and cannot tell whether
    // it already ran this call or not, especially after context
    // compression has replaced the original tool_result with a
    // summary.
    let cached_content = format!("[CACHED — this tool was already executed in a previous turn; do not call it again with the same arguments]\n{filtered}");
    let mut msg = Message::tool_result(tool_call_id, &cached_content, tool_name);
    msg.ephemeral = true;
    ctx.add_message(msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextManager;
    use hermix_core::{FunctionCall, MessageRole, ToolCall};

    /// Fresh, empty context.
    fn make_ctx() -> ContextManager {
        ContextManager::new(200_000)
    }

    /// A `terminal` tool call with no `command` arg (so the RTK
    /// terminal filter is a no-op and content passes through unchanged).
    fn make_tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: "terminal".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    /// Mimic the real call site: push the `tool_result` (as
    /// `push_tool_result_from_struct` does), then run the denial-note
    /// injection. `kind == None` produces a normal success result.
    fn run_inject(kind: Option<&str>) -> ContextManager {
        let mut ctx = make_ctx();
        let tc = make_tool_call("call_1");
        let result = match kind {
            Some(k) => ToolResult::denied("call_1", "blocked", k),
            None => ToolResult::ok("call_1", "ok output"),
        };
        push_tool_result_from_struct(&mut ctx, &[tc.clone()], &result);
        inject_denial_note_if_needed(&mut ctx, &[tc], &result);
        ctx
    }

    fn count_system_notes(ctx: &ContextManager) -> usize {
        ctx.get_messages()
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .count()
    }

    fn last_message_is_denial_note(ctx: &ContextManager) -> bool {
        ctx.get_messages()
            .last()
            .map(|m| {
                m.role == MessageRole::System
                    && m.content
                        .as_deref()
                        .unwrap_or("")
                        .contains("[SYSTEM — 工具调用被拒绝]")
            })
            .unwrap_or(false)
    }

    #[test]
    fn no_note_for_normal_success() {
        let ctx = run_inject(None);
        assert_eq!(
            count_system_notes(&ctx),
            0,
            "normal success must not inject a system note"
        );
    }

    #[test]
    fn no_note_for_policy_denial() {
        let ctx = run_inject(Some("policy"));
        assert_eq!(
            count_system_notes(&ctx),
            0,
            "policy denials are silent-by-design (no user-facing note)"
        );
    }

    #[test]
    fn injects_note_for_user_denied() {
        let ctx = run_inject(Some("user_denied"));
        assert_eq!(count_system_notes(&ctx), 1);
        assert!(last_message_is_denial_note(&ctx));
    }

    #[test]
    fn injects_note_for_timed_out() {
        let ctx = run_inject(Some("timed_out"));
        assert_eq!(count_system_notes(&ctx), 1);
        assert!(last_message_is_denial_note(&ctx));
    }

    #[test]
    fn injects_note_for_channel_closed() {
        let ctx = run_inject(Some("channel_closed"));
        assert_eq!(count_system_notes(&ctx), 1);
        assert!(last_message_is_denial_note(&ctx));
    }

    #[test]
    fn unknown_kind_falls_back_to_user_notice() {
        // `DenialKind::from_str` maps unknown strings to `ChannelClosed`,
        // which requires a user-facing note — so forward-compat denial
        // kinds still surface to the LLM rather than going silent.
        let ctx = run_inject(Some("some_future_kind"));
        assert_eq!(count_system_notes(&ctx), 1);
        assert!(last_message_is_denial_note(&ctx));
    }

    #[test]
    fn injects_note_for_permission_denied() {
        let mut ctx = make_ctx();
        let tc = make_tool_call("call_1");
        let result = ToolResult::err("call_1", "[PERMISSION_DENIED] Access is denied (os error 5)");
        push_tool_result_from_struct(&mut ctx, &[tc.clone()], &result);
        inject_permission_note_if_needed(&mut ctx, &[tc], &result);
        assert_eq!(count_system_notes(&ctx), 1);
        assert!(
            ctx.get_messages()
                .last()
                .map(|m| {
                    m.role == MessageRole::System
                        && m.content
                            .as_deref()
                            .unwrap_or("")
                            .contains("[SYSTEM — 运行环境权限/依赖受限]")
                })
                .unwrap_or(false)
        );
    }

    #[test]
    fn no_note_for_regular_execution_error() {
        let mut ctx = make_ctx();
        let tc = make_tool_call("call_1");
        let result = ToolResult::err("call_1", "No such file or directory (os error 2)");
        push_tool_result_from_struct(&mut ctx, &[tc.clone()], &result);
        inject_permission_note_if_needed(&mut ctx, &[tc], &result);
        assert_eq!(count_system_notes(&ctx), 0);
    }
}
