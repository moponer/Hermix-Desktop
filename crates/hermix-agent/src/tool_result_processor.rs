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

use hermix_core::{types::ContentPart, Message, ToolCall, ToolResult};

use crate::context::ContextManager;
use crate::terminal_filter;

/// Cumulative characters saved by RTK filtering across all tool calls
/// in this process. Exposed via `/api/status` for observability.
static FILTER_SAVED_CHARS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static FILTER_SAVED_CALLS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));

/// Counter for terminal failure hints injected this run.
/// Capped at 2 to avoid flooding the context with repeated hints.
static TERMINAL_HINT_COUNT: AtomicU64 = AtomicU64::new(0);
const MAX_TERMINAL_HINTS: u64 = 2;

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

/// Detect whether a terminal tool result content indicates a
/// shell-compatibility failure (as opposed to a command logic error).
///
/// We only inject the hint when the failure looks like a shell issue —
/// not when the command itself had a logic bug. This prevents noisy
/// hints on every non-zero exit code.
fn is_shell_compatibility_failure(content: &str) -> bool {
    let lower = content.to_lowercase();
    // Common Windows shell-compatibility error patterns
    lower.contains("not recognized as an internal or external command")
        || lower.contains("is not recognized") // PowerShell variant
        || lower.contains("command not found")
        || lower.contains("no such file or directory") && lower.contains("bash")
        || lower.contains("the system cannot find the file specified")
        || lower.contains("the system cannot find the path specified")
        || lower.contains("'cp' is not recognized")
        || lower.contains("'mv' is not recognized")
        || lower.contains("'cat' is not recognized")
        || lower.contains("'ls' is not recognized")
        || lower.contains("'rm' is not recognized")
        || lower.contains("syntax error") && lower.contains("powershell")
        || lower.contains("parsererror")
        || lower.contains("无法将") && lower.contains("识别为")
        || lower.contains("找不到")
}

/// Detect whether terminal content indicates a non-zero exit code
/// (even if `is_error` is false — terminal returns Ok with exit code
/// in the output text).
fn has_nonzero_exit_code(content: &str) -> bool {
    // Terminal output appends `[exit code: N]` at the end
    if let Some(pos) = content.rfind("[exit code: ") {
        let rest = &content[pos + "[exit code: ".len()..];
        if let Some(end) = rest.find(']') {
            let code_str = &rest[..end];
            if let Ok(code) = code_str.trim().parse::<i32>() {
                return code != 0;
            }
        }
    }
    false
}

/// If the tool result is a failed `terminal` call, append a hint
/// suggesting structured alternatives. The hint is appended directly
/// to the tool result content (not a separate system message) so the
/// LLM sees it immediately in the same context block.
///
/// Only fires for shell-compatibility failures (not logic errors),
/// and is capped at `MAX_TERMINAL_HINTS` injections per run to avoid
/// context flooding.
///
/// Returns the (possibly modified) content string.
pub fn maybe_append_terminal_failure_hint(
    content: &str,
    tool_name: Option<&str>,
    is_error: bool,
) -> String {
    // Only process terminal tool results
    if tool_name != Some("terminal") {
        return content.to_string();
    }

    // Check if this is a failure: either an explicit error, or a
    // non-zero exit code in the output text.
    let is_failure = is_error || has_nonzero_exit_code(content);
    if !is_failure {
        return content.to_string();
    }

    // Only inject for shell-compatibility failures, not logic errors
    if !is_shell_compatibility_failure(content) {
        return content.to_string();
    }

    // Cap injections to avoid flooding
    let count = TERMINAL_HINT_COUNT.fetch_add(1, Ordering::Relaxed);
    if count >= MAX_TERMINAL_HINTS {
        return content.to_string();
    }

    format!(
        "{content}\n\
         \n---\n
         [SYSTEM — terminal 命令失败提示]\n\
         该 terminal 命令因 shell 兼容性问题失败（可能是：命令不存在、引号被 Git Bash 吞掉、路径格式不兼容）。\n\
         替代方案（按优先级）：\n\
         1. **文件复制/移动/重命名** → 使用 `move_file` 工具（原生 OS API，不经过 shell，无引号/路径问题）\n\
         2. **文件读取** → 使用 `read_file` 工具\n\
         3. **文件搜索** → 使用 `search_files` 工具\n\
         4. **其他操作** → 使用 `execute_code` (Python) 替代 terminal\n\
         不要再用不同的 shell 命令变体重试同一操作。"
    )
}

/// Reset the terminal hint counter (called at the start of each run).
pub fn reset_terminal_hint_counter() {
    TERMINAL_HINT_COUNT.store(0, Ordering::Relaxed);
}

/// Push a `ToolResult` struct into context (batch path, after budget
/// pressure injection).
pub fn push_tool_result_from_struct(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    result: &ToolResult,
) {
    let tool_name = tool_calls
        .iter()
        .find(|tc| tc.id == result.tool_call_id)
        .map(|tc| tc.function.name.as_str());
    let content = maybe_append_terminal_failure_hint(&result.content, tool_name, result.is_error);
    push_tool_result(ctx, tool_calls, &result.tool_call_id, &content);
}

/// Strip base64 image data from a `read_image` tool result JSON.
/// /// After the image has been injected as multimodal content
/// (`Message::user_with_image`) by `run_loop.rs`, the base64 data
/// remaining in the tool result message is **pure redundancy** — the
/// same blob is stored twice in context, easily doubling the token
/// count for each image read (5 images × ~80 KB base64 × 2 copies
/// ≈ 800 K chars ≈ 200 K tokens of wasted context).
/// /// This function replaces `_image_meta._image_data_b64` (new nested
/// format) and `_image_data` (legacy flat format) with a short
/// placeholder, preserving all other metadata (path, dimensions,
/// mime_type, etc.) so the LLM still knows what image was loaded.
/// /// Returns the original content unchanged if:
/// - The content is not valid JSON
/// - The JSON doesn't contain image data keys
pub fn strip_image_base64(content: &str) -> String {
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };

    let mut stripped = false;

    // New nested format: _image_meta._image_data_b64
    if let Some(meta) = parsed.get_mut("_image_meta").and_then(|v| v.as_object_mut()) {
        if meta.contains_key("_image_data_b64") {
            meta.insert(
                "_image_data_b64".to_string(),
                serde_json::Value::String(
                    "[stripped — injected as multimodal content]".to_string(),
                ),
            );
            stripped = true;
        }
    }

    // Legacy flat format: _image_data
    if parsed.get("_image_data").is_some() {
        parsed["_image_data"] = serde_json::Value::String(
            "[stripped — injected as multimodal content]".to_string(),
        );
        stripped = true;
    }

    if stripped {
        serde_json::to_string(&parsed).unwrap_or_else(|_| content.to_string())
    } else {
        content.to_string()
    }
}

/// Strip `ImageUrl` from all existing multimodal user messages in the
/// context, keeping only their `Text` parts.
/// /// # First-principles rationale
/// /// An image's base64 payload (20–80 KB) has **zero marginal information
/// value** once the LLM has analysed it — the analysis text already lives
/// in the assistant message that followed. Keeping the base64 is
/// information-theoretically equivalent to keeping a raw bitmap after
/// saving a JPEG: pure waste of the scarcest resource (context window).
/// /// # When to call
/// /// Called in `prepare_assistant_turn()` (session_lifecycle.rs) **after**
/// the LLM has produced its response. By this point the LLM has already
/// analysed every image in context, so all `ImageUrl` base64 payloads
/// have zero marginal information value. The analysis lives in the
/// assistant reply; file paths are preserved in the `Text` parts so the
/// LLM can `read_image` again if it needs more detail.
/// /// # Parallel tool calls
/// /// When multiple `read_image` calls execute in the same batch, all
/// images are injected into context (without stripping each other) so
/// the LLM can see them all simultaneously. After the LLM responds,
/// this function strips ALL of them at once.
/// /// # Token accounting
/// /// After stripping, `ctx.recompute_total_tokens()` is called so the
/// cached running total reflects the removed base64. Without this the
/// pressure threshold would remain artificially high until the next
/// `add_message` call.
/// /// Returns the number of image URLs stripped.
pub fn strip_previous_image_urls(ctx: &mut ContextManager) -> usize {
    let mut stripped_count = 0usize;
    let mut any_changed = false;

    for msg in ctx.get_messages_mut().iter_mut() {
        let Some(parts) = msg.content_parts.as_mut() else {
            continue;
        };
        let had_image = parts.iter().any(|p| matches!(p, ContentPart::ImageUrl { .. }));
        if !had_image {
            continue;
        }

        // Rebuild parts: keep all Text, drop all ImageUrl, append a
        // short note so the LLM knows the image was analysed and
        // the base64 was removed to save context.
        let mut kept: Vec<ContentPart> = Vec::with_capacity(parts.len() + 1);
        for part in parts.drain(..) {
            match part {
                ContentPart::Text { text } => {
                    if !text.is_empty() {
                        kept.push(ContentPart::Text { text });
                    }
                }
                ContentPart::ImageUrl { .. } => {
                    stripped_count += 1;
                }
            }
        }
        kept.push(ContentPart::Text {
            text: "[image base64 removed to save context — \
                   analysis is in the assistant reply above; \
                   re-call read_image if you need to see it again]"
                .to_string(),
        });
        msg.content_parts = Some(kept);
        any_changed = true;
    }

    if any_changed {
        ctx.recompute_total_tokens();
    }

    stripped_count
}

/// Look up the tool name for `tool_call_id` from `tool_calls`, then
/// push `Message::tool_result(id, content, tool_name)` into `ctx`.
/// For terminal tools, applies RTK-style command-aware output filtering.
/// For `read_image` results, strips base64 image data before pushing —
/// the image is injected separately as multimodal content by
/// `run_loop.rs`, so keeping the full base64 in the tool result
/// message would double the context cost per image.
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

    // Strip base64 from read_image tool results. Under the zero-
    // context-overhead model (v2), the main agent must
    // NEVER see image base64 — it should delegate analysis to a
    // sub-agent via `delegate_task`. The base64 in the raw tool
    // result would bloat the main context, so we strip it here.
    // The tool result still contains the file path and metadata so
    // the LLM can construct the `delegate_task` call.
    let final_content = if tool_name == Some("read_image") {
        strip_image_base64(&filtered)
    } else {
        filtered
    };

    ctx.add_message(Message::tool_result(tool_call_id, &final_content, tool_name));
}

/// (A+C product fix): when a tool call was **denied** (not
/// merely failed on execution) — user拒绝 / 超时 / 连接中断 — inject a
/// structured `system` note right after the `tool_result` so the LLM's
/// next turn is forced to (1) tell the user the call was blocked, (2)
/// explain why the tool was needed, and (3) ask how to proceed instead
/// of silently retrying or going quiet. Policy denials
/// (`denial_kind == "policy"`) are intentionally skipped — the LLM
/// already knows the policy gate fired from the `reason` string and a
/// second note would be noise.
/// /// No-op for normal results (`denial_kind == None`) and execution
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

/// (sandbox-permission fix P3): when a tool failed with a
/// `PERMISSION_DENIED` error (filesystem ACL / sandbox virtualization /
/// missing admin / missing Windows OCR language pack / no interactive desktop
/// session, tagged `[PERMISSION_DENIED]` by `ToolError::from_message`), inject
/// a structured `system` note right after the `tool_result` so the LLM's next
/// turn is forced to (1) tell the user the action was blocked by the runtime,
/// (2) explain what access it needed, and (3) ask the user to grant that
/// access in the runtime — instead of silently retrying, treating virtualized
/// data as real, or fabricating a generic "Windows cleanup checklist" to paper
/// over the failure.
/// /// This enforces the boss philosophy: Hermix runs on a real Windows machine;
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

/// push a `tool_result` message that exists **only**
/// in the in-memory context — it is never persisted to the DB.
/// Used by the replay path where the tool wasn't actually re-executed;
/// the message is needed to satisfy the provider API's `tool_call_id`
/// pairing requirement, but the original execution's `tool_result` is
/// already persisted. Creating a second non-ephemeral row would produce
/// duplicate cards on the frontend after a session reload.
/// /// the content is prefixed with a `[CACHED]` marker
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
