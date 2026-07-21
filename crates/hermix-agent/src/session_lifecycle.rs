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
//!   `EmptyResponseAction` match centralisation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermix_core::{
    AgentError, AgentResult, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk, ToolCall,
    UsageStats,
};
use hermix_focus::{FocusStack, MemoryRouter, SessionFrame};

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::agent_loop::PostLlmAction;
use crate::compression::CompressionMetrics;
use crate::context::ContextManager;
use crate::agent_loop::RouteState;
use crate::cost_guard::CostGuard;
use crate::message_cache::MessageCache;
use crate::message_helpers::{
    assistant_has_reasoning, assistant_visible_text, emit_status, extra_body_for_api_mode,
};
use crate::plugins::PluginManager;

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
///   `decide_empty_response` to know whether tool execution
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
.map(|s| s.trim().to_string())
.filter(|s| !s.is_empty()),
);
    }
    (history_includes_tool, assistant_msg, tool_calls)
}

/// Phase 2.1 (2026-07-03): apply the four-variant `EmptyResponseAction`
/// outcome produced by
/// `crate::empty_response_handler::decide_empty_response`.
/// Both `run()` and `run_stream()` previously open-coded this match
/// arm — the only difference between the two call sites was the
/// tracing message text. This helper centralises the match and
/// parameterises the stream-mode log suffix.
///
/// Returns `true` when a `ContinueWithUserMsg` or
/// `ContinueWithSystemNudge` variant injected a message into the
/// context — the caller should `continue;` to restart the loop with
/// the injected message. Returns `false` when the variant was
/// `Finalize` (a no-op) — the caller should fall
/// through to its terminal finalize path.
pub fn apply_fallback_outcome(
    ctx: &mut ContextManager,
    action: crate::empty_response_handler::EmptyResponseAction,
    stream_mode: bool,
    total_turns: u32,
    autonomous_nudge_count: u32,
) -> bool {
    use crate::empty_response_handler::EmptyResponseAction;
    match action {
        EmptyResponseAction::ContinueWithUserMsg(msg) => {
            ctx.add_message(Message::user(&msg));
            true
        }
EmptyResponseAction::ContinueWithSystemNudge(msg) => {
tracing::debug!(
nudge_count = autonomous_nudge_count,
total_turns,
"autonomous_mode{}: nudging LLM back into action",
if stream_mode { "" } else { " [non-stream]" }
);
ctx.add_message(Message::system(&msg));
true
}
EmptyResponseAction::Finalize => false,
}
}

/// Phase 1.3a (2026-07-03): the post-LLM logic that was duplicated
/// ~200 lines in `run()` and `run_stream()`. Fires the PostLlmCall
/// hook, accumulates usage, runs the cost guard (with auto-degrade
/// to the configured cheap model), and decides between
/// `Finalize` (no tool calls → caller returns) and `Continue`
/// (tool calls → caller falls through to `execute_tool_calls`).
///
/// `stream_mode` is `true` for `run_stream()` — the helper uses
/// this to emit a stream-specific `tracing` line and to forward
/// the `finish_reason` through the finalize path.
///
/// Phase 5 (2026-07-03): 7 mutable scalar args collapsed to
/// `&mut CostGuard` + `&mut Option<String>` (4 args shrunk to 1
/// struct). `record_usage` / `evaluate_thresholds` / `install_downgrade`
/// on [`CostGuard`] absorb the 4-field atomic-mutation, so the
/// helper signature dropped from 11 args to 7.
///
/// Phase 5 (2026-07-04 follow-up): `&mut Option<String>` for
/// `last_content_with_tools` collapsed to `&mut MessageCache`,
/// dropping the helper to 6 mutable args (the `Option<String>` is
/// now reached via `cache.store_content_with_tools(...)` inside
/// the helper). The companion helper `prepare_assistant_turn`
/// also takes `&mut MessageCache` for the same reason.
///
/// Phase 2.x (2026-07-04): extracted to this module so `agent_loop.rs`
/// can shrink. Callers pass `cost_degrade_model` and
/// `turn_route_cost_guard` closures that capture `&AgentLoop`,
/// since those helpers still live on the type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_post_response<F, G>(
    config: &AgentConfig,
    plugin_manager: Option<&Arc<Mutex<PluginManager>>>,
    ctx: &mut ContextManager,
    response: &LlmResponse,
    turn_usage_acc: Option<&UsageStats>,
    total_turns: u32,
    api_elapsed: u64,
    cost_guard: &mut CostGuard,
    route_state: &mut RouteState,
    message_cache: &mut MessageCache,
    stream_mode: bool,
    cost_degrade_model: F,
    turn_route_cost_guard_fn: G,
) -> Result<crate::agent_loop::PostResponseAction, AgentError>
where
    F: FnOnce() -> Option<String>,
    G: FnOnce(String) -> crate::agent_loop::TurnRuntimeRoute,
{
    // 2026-07-08: check for tool calls with NON-EMPTY function names.
    // MiniMax M2.7 streaming can create placeholder tool call entries
    // whose `name` field is empty (the name delta never arrived, or
    // the tool call XML was already stripped by the sanitizer). Without
    // this check, `has_tool_calls` would be `true` for a vec of
    // empty-named entries, causing `process_post_response` to return
    // `Continue` — but then `run_stream` filters them out and falls
    // into the `decide_empty_response` path, which fires a nudge that
    // re-drives the LLM to re-run the same tools.
    let has_tool_calls = response
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|tc| tc.iter().any(|t| !t.function.name.is_empty()));

    if stream_mode {
        // 2026-07-21 (diagnostic): the previous single-line
        // `llm_response_received` only logged `has_tool_calls` (a boolean
        // derived from "any name non-empty") which is ambiguous when
        // MiniMax streaming leaves behind placeholder entries with
        // empty `name` fields. After this addition we can tell at a
        // glance whether the LLM actually emitted a dispatchable tool
        // call (named_tool_calls > 0) vs only placeholders
        // (tool_calls_vec_len > 0 but named_tool_calls == 0) vs none
        // at all. The next time we see "run suddenly stops mid-stream"
        // (master: 跑着就停了), this line tells us whether the model
        // actually returned something we could dispatch or whether it
        // bounced out via decide_empty_response.
        let tool_calls_vec_len = response
            .message
            .tool_calls
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0);
        let named_tool_calls = response
            .message
            .tool_calls
            .as_ref()
            .map(|v| {
                v.iter()
                    .filter(|t| !t.function.name.is_empty())
                    .map(|t| t.function.name.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tool_call_names: Vec<&str> = named_tool_calls.clone();
        tracing::info!(
            turn = total_turns,
            has_tool_calls,
            tool_calls_vec_len,
            named_tool_calls = tool_calls_vec_len > 0 && named_tool_calls.is_empty(),
            tool_call_names = ?tool_call_names,
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            "llm_response_received"
        );
    }

    // --- Post-LLM hook ---
    let post_ctx = serde_json::json!({
        "turn": total_turns,
        "api_time_ms": api_elapsed,
        "has_tool_calls": has_tool_calls,
    });
    let post_results = crate::hook_dispatch::invoke_hook(
        plugin_manager,
        crate::plugins::HookType::PostLlmCall,
        &post_ctx,
    );
    crate::hook_dispatch::inject_hook_context(&post_results, ctx);

    cost_guard.record_usage(turn_usage_acc, response.model.as_str(), config);

    // --- Cost guard ---
    let action = cost_guard.evaluate_thresholds(
        route_state,
        config.max_cost_usd,
        config.cost_guard_degrade_at_ratio,
        cost_degrade_model,
    );
    // Two independent side effects: warn (system message, route
    // downgrade) and trip (system message, terminate loop). Both
    // can fire on the same call when spend jumped past both
    // thresholds in one turn — fire warn first, then trip, so the
    // user sees "switching to cheaper model" before the
    // terminating "stopping loop" message.
    if let Some(warn) = action.warn {
        match warn.kind {
            crate::cost_guard::WarnKind::Downgrade { model } => {
                let route = turn_route_cost_guard_fn(model.clone());
                route_state.install_downgrade(route);
                ctx.add_message(Message::system(format!(
                    "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                    warn.session_cost_usd, warn.limit, model
                )));
            }
            crate::cost_guard::WarnKind::Warn => {
                ctx.add_message(Message::system(format!(
                    "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                    warn.session_cost_usd, warn.limit
                )));
            }
        }
    }
    if let Some(trip) = action.trip {
        ctx.add_message(Message::system(format!(
            "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
            trip.session_cost_usd, trip.limit
        )));
        return Ok(crate::agent_loop::PostResponseAction::CostGuardTripped);
    }

    // Use the same non-empty-name check as `has_tool_calls` above.
    let tool_calls_present = response
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|tc| tc.iter().any(|t| !t.function.name.is_empty()));

    if !tool_calls_present {
        // No tool calls — agent is done. Save the last assistant
        // content for ack-continuation tracking.
if response
.message
.content
.as_deref()
.is_some_and(|s| !s.is_empty())
{
message_cache.store_content_with_tools(
response
.message
.content
.as_deref()
.map(|s| s.trim().to_string())
.filter(|s| !s.is_empty()),
);
        }
        return Ok(crate::agent_loop::PostResponseAction::Finalize);
    }

    // Tool calls present — caller continues to dispatch.
    Ok(crate::agent_loop::PostResponseAction::Continue)
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
    memory_callbacks: Option<&AgentCallbacks>,
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
    crate::memory_prefetch::memory_on_session_end(config, memory_router, focus_stack, messages, memory_callbacks);
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

/// Per-attempt classification inside the retry loop (shared by
/// `run()` and `run_stream()`). Decides whether the LLM response
/// warrants a retry (empty content) or breaks out.
///
/// 2026-07-09: Removed `RetryReasoning` (thinking-only inner-loop
/// retry). When the LLM returns only `reasoning_content` with no
/// visible text and no tool calls, we break immediately and let
/// the outer loop's `decide_empty_response` handle it with a
/// single post-tool nudge. The inner-loop retry was
/// counterproductive: it added the reasoning-only message to
/// context, so the LLM saw its own "answer" and returned
/// reasoning-only again on every retry.
pub(crate) fn classify_post_llm_response(
    config: &AgentConfig,
    callbacks: &AgentCallbacks,
    r: LlmResponse,
    _inner_thinking: &mut u32,
    inner_empty: &mut u32,
    _ctx: &mut ContextManager,
) -> PostLlmAction {
    let has_tools = r
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|tc| !tc.is_empty());
    if has_tools {
        return PostLlmAction::Break(r);
    }
    if assistant_visible_text(&r.message) {
        return PostLlmAction::Break(r);
    }
    // Reasoning-only (no visible text, no tools): break immediately.
    // The outer loop's `decide_empty_response` will decide whether
    // to nudge the LLM or finalize. No inner-loop retry.
    if assistant_has_reasoning(&r.message) {
        return PostLlmAction::Break(r);
    }
    // Truly empty (no reasoning, no visible text, no tools): retry.
    if *inner_empty < config.empty_content_max_retries
    {
        *inner_empty += 1;
        tracing::warn!(
            "empty assistant response — retrying ({}/{})",
            *inner_empty,
            config.empty_content_max_retries
        );
        emit_status(
            config,
            callbacks,
            "lifecycle",
            &format!(
                "Empty assistant response — retrying ({}/{})",
                *inner_empty, config.empty_content_max_retries
            ),
        );
        return PostLlmAction::RetryEmpty;
    }
    PostLlmAction::Break(r)
}

/// Ask the LLM for a final summary when the turn budget is exhausted.
///
/// If the summary LLM call itself fails (e.g. context too long, rate
/// limit, provider error), we gracefully return `None` instead of
/// propagating the error. This prevents the max-turns guard from
/// turning into a hard error that the frontend misinterprets as
/// "模型不可用". The agent already has a full conversation history;
/// finalizing without a summary is better than showing an error.
pub async fn handle_max_iterations(
    config: &AgentConfig,
    llm_provider: &Arc<dyn LlmProvider>,
    ctx: &mut ContextManager,
) -> Result<Option<Message>, AgentError> {
    ctx.add_message(Message::system(
        "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
         what was accomplished and any remaining tasks.",
    ));
    match llm_provider
        .chat_completion(
            ctx.get_messages(),
            &[],
            config.max_tokens,
            config.temperature,
            Some(config.model.as_str()),
            extra_body_for_api_mode(config, &config.api_mode).as_ref(),
        )
        .await
    {
        Ok(response) => Ok(Some(response.message)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "handle_max_iterations: summary LLM call failed; finalizing without summary"
            );
            Ok(None)
        }
    }
}