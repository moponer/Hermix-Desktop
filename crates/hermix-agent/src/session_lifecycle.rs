//! Session-lifecycle helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No
//! behavior change** — pure relocation.
//!
//! Items living here:
//! - `hydrate_stored_system_prompt_from_hermes_home` — read
//!   `sessions.system_prompt` from SQLite for gateway/CLI session
//!   continuation.
//! - `should_emit_session_reset_hook` — detect a `/reset` or `/new`
//!   user command at the tail of the message log.
//! - `messages_for_persisted_result` — apply the
//!   `persist_user_message` override before returning/persisting.
//! - `emit_mute_transition` — centralised `mute_post_response`
//!   control-chunk emit (only fires on transition).
//! - `prepare_assistant_turn` — Phase 2.2 per-turn post-LLM setup
//!   shared between `run()` and `run_stream()`.
//! - `apply_fallback_outcome` — Phase 2.1 four-variant
//!   `PostResponseFallback` match centralisation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermix_core::{
    AgentError, AgentResult, LlmResponse, Message, MessageRole, StreamChunk, ToolCall, UsageStats,
};
use hermix_focus::{FocusStack, MemoryRouter, SessionFrame};

use crate::agent_config::AgentConfig;
use crate::compression::CompressionMetrics;
use crate::context::ContextManager;
use crate::message_cache::MessageCache;
use crate::plugins::PluginManager;
use crate::python_alignment::strip_think_blocks_for_ack;

/// Populate [`AgentConfig::stored_system_prompt`] from SQLite
/// (`sessions.system_prompt`) for session continuation.
///
/// Call before [`AgentLoop::run`] when resuming a gateway/CLI session
/// so Anthropic prefix cache matches Python.
pub fn hydrate_stored_system_prompt_from_hermes_home(
    config: &mut AgentConfig,
    hermix_home: &std::path::Path,
) -> Result<(), hermix_core::AgentError> {
    let Some(ref sid) = config.session_id else {
        return Ok(());
    };
    if sid.trim().is_empty() {
        return Ok(());
    }
    let sp = crate::session_persistence::SessionPersistence::new(hermix_home);
    sp.ensure_db()?;
    if let Some(prompt) = sp.get_system_prompt(sid)? {
        config.stored_system_prompt = Some(prompt);
    }
    Ok(())
}

/// Returns `true` when the last user message in `messages` is a
/// `/reset` or `/new` command — used by `finalize_agent_result` to
/// fire `OnSessionReset` after the session-end hooks.
pub fn should_emit_session_reset_hook(messages: &[Message]) -> bool {
    messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .and_then(|m| m.content.as_deref())
        .map(|content| {
            let trimmed = content.trim();
            matches!(trimmed, "/reset" | "/new")
        })
        .unwrap_or(false)
}

/// Build the [`Message`] vector for return / persistence (applies the
/// `persist_user_message` override when supplied).
pub fn messages_for_persisted_result(
    config: &AgentConfig,
    ctx: &ContextManager,
    persist_user_idx: Option<usize>,
) -> Vec<Message> {
    let mut msgs = ctx.get_messages().to_vec();
    if let (Some(idx), Some(override_text)) = (
        persist_user_idx,
        config.persist_user_message.as_deref(),
    ) {
        if let Some(msg) = msgs.get_mut(idx) {
            if msg.role == MessageRole::User {
                msg.content = Some(override_text.to_string());
            }
        }
    }
    msgs
}

/// Phase 2.3 (2026-07-03): centralised `mute_post_response` control
/// chunk emit. The same AtomicBool-swap-then-conditional-emit pattern
/// was open-coded in two places inside `run_stream()`. This helper
/// takes the stream callback, the mute flag, and the desired next
/// state, and only fires the control chunk when the state actually
/// changes (the frontend only needs to know about transitions, not
/// every dispatch).
pub fn emit_mute_transition(
    on_chunk: &Box<dyn Fn(StreamChunk) + Send + Sync>,
    stream_mute: &AtomicBool,
    next_muted: bool,
) {
    let was_muted = stream_mute.swap(next_muted, Ordering::AcqRel);
    if was_muted != next_muted {
        on_chunk(StreamChunk {
            delta: Some(hermix_core::StreamDelta {
                content: None,
                tool_calls: None,
                extra: Some(serde_json::json!({
                    "control": "mute_post_response",
                    "enabled": next_muted
                })),
            }),
            finish_reason: None,
            usage: None,
        });
    }
}

/// Phase 2.2 (2026-07-03): shared per-turn post-LLM setup that `run()`
/// and `run_stream()` previously open-coded after the
/// `process_post_response` call. Builds the local `assistant_msg`,
/// appends it to the context, updates `last_content_with_tools` if
/// the assistant content came bundled with tool calls (used by the
/// codex-ack continuation logic on the next turn), and reports
/// whether any prior tool result exists in the history.
///
/// Returns the three locals the caller needs:
/// - `history_includes_tool`: did any earlier turn leave a `Tool`
///   message in the context? Used by
///   `decide_post_response_fallback` to know whether tool execution
///   is part of the protocol context.
/// - `assistant_msg`: the LLM's response, freshly appended to the
///   context.
/// - `tool_calls`: the LLM's claimed tool calls, cloned for the
///   caller to dispatch / dedup.
#[allow(clippy::type_complexity)]
pub fn prepare_assistant_turn(
    ctx: &mut ContextManager,
    response: &LlmResponse,
    message_cache: &mut MessageCache,
) -> (bool, Message, Option<Vec<ToolCall>>) {
    let history_includes_tool = ctx
        .get_messages()
        .iter()
        .any(|m| m.role == MessageRole::Tool);
    let assistant_msg = response.message.clone();
    let tool_calls = assistant_msg.tool_calls.clone();
    ctx.add_message(assistant_msg.clone());
    if assistant_msg
        .tool_calls
        .as_ref()
        .is_some_and(|v| !v.is_empty())
        && crate::message_helpers::assistant_visible_text_after_think_blocks(&assistant_msg)
    {
        message_cache.store_content_with_tools(
            assistant_msg
                .content
                .as_deref()
                .map(strip_think_blocks_for_ack)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    (history_includes_tool, assistant_msg, tool_calls)
}

/// Phase 2.1 (2026-07-03): apply the four-variant `PostResponseFallback`
/// outcome produced by
/// `crate::post_response_fallback::decide_post_response_fallback`.
/// Both `run()` and `run_stream()` previously open-coded this match
/// arm — the only difference between the two call sites was the
/// tracing message text. This helper centralises the match and
/// parameterises the stream-mode log suffix.
///
/// Returns `true` when a `ContinueWithUserMsg` or
/// `ContinueWithSystemNudge` variant injected a message into the
/// context — the caller should `continue;` to restart the loop with
/// the injected message. Returns `false` when the variant was
/// `PatchLastAssistant` (which mutated the last assistant message
/// in place) or `Finalize` (a no-op) — the caller should fall
/// through to its terminal finalize path.
pub fn apply_fallback_outcome(
    ctx: &mut ContextManager,
    action: crate::post_response_fallback::PostResponseFallback,
    stream_mode: bool,
    total_turns: u32,
    autonomous_nudge_count: u32,
) -> bool {
    use crate::post_response_fallback::PostResponseFallback;
    match action {
        PostResponseFallback::ContinueWithUserMsg(msg) => {
            ctx.add_message(Message::user(&msg));
            true
        }
        PostResponseFallback::ContinueWithSystemNudge(msg) => {
            tracing::debug!(
                nudge_count = autonomous_nudge_count,
                total_turns,
                "autonomous_mode{}: nudging LLM back into action",
                if stream_mode { "" } else { " [non-stream]" }
            );
            ctx.add_message(Message::system(&msg));
            true
        }
        PostResponseFallback::PatchLastAssistant { content } => {
            if let Some(last) = ctx.get_messages_mut().last_mut() {
                if last.role == MessageRole::Assistant {
                    last.content = Some(content);
                }
            }
            false
        }
        PostResponseFallback::Finalize => false,
    }
}

/// Phase 2.x (2026-07-04): consolidate the session-end side effects
/// that fire at the end of every `run()` / `run_stream()` invocation:
///
/// 1. Build the session-end `Value` and fire `OnSessionEnd`.
/// 2. Optionally fire `OnSessionReset` when the last user message was
///    a `/reset` or `/new` command.
/// 3. Route `memory_on_session_end` (persist session-level
///    `FocusConclusion`, drain pending conclusions, call
///    `MemoryRouter::on_session_end`).
/// 4. Fire `OnSessionFinalize` (final turn count + cost snapshot).
/// 5. P1-3: persist accumulated token usage so `/insights` can chart
///    real numbers. Best-effort; failure logged but non-fatal.
///
/// All hooks are fire-and-forget (`let _ =`); failures never break
/// the response. The same `finalize_agent_result` is also called by
/// `graceful_interrupt_result` for non-natural exits.
#[allow(clippy::too_many_arguments)]
pub fn finalize_agent_result(
    config: &AgentConfig,
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    memory_router: Option<&Arc<MemoryRouter>>,
    plugin_manager: Option<&Arc<Mutex<PluginManager>>>,
    latest_compressed_summary: &Mutex<Option<String>>,
    latest_compression_archived_turns: &Mutex<Option<Vec<Message>>>,
    latest_compression_metrics: &Mutex<Option<CompressionMetrics>>,
    ctx: &ContextManager,
    total_turns: u32,
    tool_errors: &[hermix_core::ToolErrorRecord],
    accumulated_usage: Option<UsageStats>,
    session_cost_usd: f64,
    session_started_hooks_fired: bool,
    persist_user_idx: Option<usize>,
    finished_naturally: bool,
    interrupted: bool,
    finish_reason: Option<String>,
) -> AgentResult {
    let messages = ctx.get_messages();
    let _ = crate::hook_dispatch::invoke_hook(
        plugin_manager,
        crate::plugins::HookType::OnSessionEnd,
        &serde_json::json!({
            "session_id": config.session_id,
            "turns": total_turns,
            "finished_naturally": finished_naturally,
            "interrupted": interrupted,
            "session_started_hooks_fired": session_started_hooks_fired,
        }),
    );
    if should_emit_session_reset_hook(messages) {
        let _ = crate::hook_dispatch::invoke_hook(
            plugin_manager,
            crate::plugins::HookType::OnSessionReset,
            &serde_json::json!({
                "session_id": config.session_id,
                "turns": total_turns,
                "source": "user_command",
            }),
        );
    }
    crate::memory_prefetch::memory_on_session_end(config, memory_router, focus_stack, messages);
    let _ = crate::hook_dispatch::invoke_hook(
        plugin_manager,
        crate::plugins::HookType::OnSessionFinalize,
        &serde_json::json!({
            "session_id": config.session_id,
            "turns": total_turns,
            "tool_errors": tool_errors.len(),
            "session_cost_usd": session_cost_usd,
        }),
    );

    // P1-3 (2026-06-27): persist the final accumulated token usage
    // to `token_usage` so `/insights` can chart real numbers. We do
    // this *after* the session-end hooks fire (so a hook can still
    // mutate usage if needed) and *before* we build the `AgentResult`
    // (so callers see the same numbers in `result.usage` that we
    // just wrote to disk). Best-effort: a failed insert never
    // breaks the response.
    if let (Some(sid), Some(usage)) = (config.session_id.as_ref(), accumulated_usage.as_ref()) {
        if !sid.trim().is_empty() && (usage.prompt_tokens > 0 || usage.completion_tokens > 0) {
            let home_buf = config
                .hermix_home
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(hermix_config::paths::hermix_home);
            let sp = crate::session_persistence::SessionPersistence::new(&home_buf);
            if let Err(e) = sp.persist_token_usage(sid, &config.model, usage) {
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    "P1-3: failed to persist token_usage (non-fatal); /insights will show stale data"
                );
            }
        }
    }

    AgentResult {
        messages: messages_for_persisted_result(config, ctx, persist_user_idx),
        finished_naturally,
        total_turns,
        tool_errors: tool_errors.to_vec(),
        usage: accumulated_usage,
        interrupted,
        session_cost_usd: Some(session_cost_usd),
        session_started_hooks_fired,
        finish_reason,
        compressed_summary: latest_compressed_summary
            .lock()
            .ok()
            .and_then(|g| g.clone()),
        compression_archived_turns: latest_compression_archived_turns
            .lock()
            .ok()
            .and_then(|g| g.clone()),
        compression_metrics: latest_compression_metrics
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|m| m.to_snapshot())),
    }
}