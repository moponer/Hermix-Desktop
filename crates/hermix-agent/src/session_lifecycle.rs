//! Session-lifecycle helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
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
//! - `prepare_assistant_turn` — per-turn post-LLM setup
//!   shared between `run()` and `run_stream()`.
//! - `apply_fallback_outcome` — four-variant
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
use crate::agent_loop::RouteState;
use crate::compression::CompressionMetrics;
use crate::context::ContextManager;
use crate::cost_guard::CostGuard;
use crate::message_cache::MessageCache;
use crate::message_helpers::{
    assistant_has_reasoning, assistant_visible_text, emit_status, extra_body_for_api_mode,
};
use crate::plugins::PluginManager;

/// Populate [`AgentConfig::stored_system_prompt`] from SQLite
/// (`sessions.system_prompt`) for session continuation.
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

/// Append a synthetic `tool` result for every `tool_call` that never
/// received one.
///
/// Why this has to happen: most providers reject a request whose history
/// contains an assistant message with `tool_calls` but no following
/// `tool` message for each call ("tool call result does not follow tool
/// call", 400). `GenericProvider` papers over this at request time with
/// `strip_unanswered_tool_calls`, but `AnthropicProvider` has no such
/// repair pass — an unanswered `tool_use` block is a live failure there.
/// The same dangling state also reaches SQLite through
/// `AgentResult.messages`, so a reloaded session would 400 forever.
///
/// Runs here rather than mutating `ctx` because this is the last point
/// where the final message vector is materialised; every termination
/// path in `run()` / `run_stream()` funnels through
/// `AgentLoop::finalize_agent_result` -> this function, so no path can
/// slip past.
///
/// The synthetic text tells the model the call may have had side
/// effects, so it does not blindly retry a `write_file` that in fact
/// completed.
fn backfill_unanswered_tool_results(messages: &mut Vec<Message>) -> usize {
    use std::collections::HashSet;

    // Collect ids already answered anywhere in the history. A `tool`
    // message is the only thing that counts as an answer; a later
    // assistant turn does not "consume" an earlier pending call.
    let answered: HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();

    let mut insertions: Vec<(usize, Message)> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        let Some(tool_calls) = msg.tool_calls.as_ref() else {
            continue;
        };
        if tool_calls.is_empty() {
            continue;
        }
        for tc in tool_calls {
            if answered.contains(tc.id.as_str()) {
                continue;
            }
            insertions.push((
                idx,
                Message::tool_result(
                    &tc.id,
                    format!(
                        "This tool call produced no result — the turn ended before it \
                         completed. [Do not blindly retry] It may have partially executed, \
                         so side effects may already have happened. Verify the current \
                         state before re-running `{}`.",
                        tc.function.name
                    ),
                    Some(tc.function.name.clone()),
                ),
            ));
        }
    }

    if insertions.is_empty() {
        return 0;
    }
    let count = insertions.len();
    // Insert in reverse index order so earlier insertions don't shift the
    // positions of later ones. Each synthetic result goes immediately
    // after its assistant message, which is what
    // `enforce_strict_tool_pairing` requires — appending at the tail
    // would be dropped as an orphan by the provider sanitiser.
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    for (idx, msg) in insertions {
        messages.insert(idx + 1, msg);
    }
    count
}

/// Build the [`Message`] vector for return / persistence (applies the
/// `persist_user_message` override when supplied).
pub fn messages_for_persisted_result(
    config: &AgentConfig,
    ctx: &ContextManager,
    persist_user_idx: Option<usize>,
) -> Vec<Message> {
    let mut msgs = ctx.get_messages().to_vec();
    let backfilled = backfill_unanswered_tool_results(&mut msgs);
    if backfilled > 0 {
        tracing::warn!(
            backfilled,
            "backfilled unanswered tool calls before persisting; the turn ended \
             with tool_calls that never received a result"
        );
    }
    if let (Some(idx), Some(override_text)) =
        (persist_user_idx, config.persist_user_message.as_deref())
    {
        if let Some(msg) = msgs.get_mut(idx) {
            if msg.role == MessageRole::User {
                msg.content = Some(override_text.to_string());
            }
        }
    }
    msgs
}

/// centralised `mute_post_response` control
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

/// shared per-turn post-LLM setup that `run`
/// and `run_stream()` previously open-coded after the
/// `process_post_response` call. Builds the local `assistant_msg`,
/// appends it to the context, updates `last_content_with_tools` if
/// the assistant content came bundled with tool calls (used by the
/// codex-ack continuation logic on the next turn), and reports
/// whether any prior tool result exists in the history.
/// Returns the three locals the caller needs:
/// - `history_includes_tool`: did any earlier turn leave a `Tool`
/// message in the context? Used by
/// `decide_empty_response` to know whether tool execution
/// is part of the protocol context.
/// - `assistant_msg`: the LLM's response, freshly appended to the
/// context.
/// - `tool_calls`: the LLM's claimed tool calls, cloned for the
/// caller to dispatch / dedup.
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

    //── Image base64 lifecycle (, revised) ──────
    // Safety-net cleanup for image base64. Under the zero-context-
    // overhead model (v2), `read_image` is never called
    // directly in the main context — all image analysis is delegated
    // to sub-agents via `delegate_task`. The sub-agent's isolated
    // context absorbs the base64 cost and is destroyed when done.
    // This strip call is retained as a safety net in case:
    // - A user pastes/attaches an inline image (which enters the
    // main context as a `user_with_image` message).
    // - A legacy code path injects an image message.
    // Under normal operation with the new model, there should be
    // nothing to strip here.
    let stripped = crate::tool_result_processor::strip_previous_image_urls(ctx);
    if stripped > 0 {
        tracing::debug!(
            stripped_count = stripped,
            "stripped ImageUrl from {} message(s) after assistant response — \
             image base64 no longer needed (LLM has already analysed it)",
            stripped,
        );
    }

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

/// apply the three-variant `EmptyResponseAction`
/// outcome produced by
/// `crate::empty_response_handler::decide_empty_response`.
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

/// .3a : the post-LLM logic that was duplicated
/// ~200 lines in `run()` and `run_stream()`. Fires the PostLlmCall
/// hook, accumulates usage, runs the cost guard (with auto-degrade
/// to the configured cheap model), and decides between
/// `Finalize` (no tool calls → caller returns) and `Continue`
/// (tool calls → caller falls through to `execute_tool_calls`).
/// `stream_mode` is `true` for `run_stream()` — the helper uses
/// this to emit a stream-specific `tracing` line and to forward
/// the `finish_reason` through the finalize path.
/// 7 mutable scalar args collapsed to
/// `&mut CostGuard` + `&mut Option<String>` (4 args shrunk to 1
/// struct). `record_usage` / `evaluate_thresholds` / `install_downgrade`
/// on [`CostGuard`] absorb the 4-field atomic-mutation, so the
/// helper signature dropped from 11 args to 7.
/// (follow-up): `&mut Option<String>` for
/// `last_content_with_tools` collapsed to `&mut MessageCache`,
/// dropping the helper to 6 mutable args (the `Option<String>` is
/// now reached via `cache.store_content_with_tools(...)` inside
/// the helper). The companion helper `prepare_assistant_turn`
/// also takes `&mut MessageCache` for the same reason.
/// .x : extracted to this module so `agent_loop.rs`
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
    acceptance_gate: &mut crate::acceptance_gate::AcceptanceGate,
    stream_mode: bool,
    cost_degrade_model: F,
    turn_route_cost_guard_fn: G,
) -> Result<crate::agent_loop::PostResponseAction, AgentError>
where
    F: FnOnce() -> Option<String>,
    G: FnOnce(String) -> crate::agent_loop::TurnRuntimeRoute,
{
    // check for tool calls with NON-EMPTY function names.
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
        // (diagnostic): the previous single-line
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

    // --- Acceptance gate (advisory) ---
    //
    // The gate checks the reply before it is committed. In production
    // its profile arms **no** Reject check, so this never drops or
    // rewrites a reply and never spends an extra API call: a warning
    // is injected into the next turn's context, where the model can
    // correct itself (the Cursor "self-correct in the next turn"
    // pattern). The check it does own is the one nothing else covers —
    // the model asserting an action it never performed.
    run_acceptance_gate(
        acceptance_gate,
        ctx,
        response,
        tool_calls_present,
        total_turns,
    );

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

/// Run the acceptance gate over the assistant's reply and inject any
/// advisory into the context for the next turn.
///
/// **Advisory only.** The production profile arms no `Reject` check, so
/// this function never blocks a reply and never triggers a retry. A
/// warning becomes a system message in the context, which the model
/// sees on its next call — enough for it to correct itself, without
/// paying for an extra round trip.
///
/// The gate's own check (the "unbacked completion claim") needs three
/// signals, so it only fires when the model claimed an action that
/// nothing in the session could have performed:
///
/// 1. the user asked for a mutating action,
/// 2. the reply asserts that action was done, and
/// 3. no tool ever ran — no tool result in the history, no tool call
///    in this turn.
fn run_acceptance_gate(
    gate: &mut crate::acceptance_gate::AcceptanceGate,
    ctx: &mut ContextManager,
    response: &LlmResponse,
    tool_calls_present: bool,
    total_turns: u32,
) {
    let visible = crate::python_alignment::strip_think_blocks_for_ack(
        response.message.content.as_deref().unwrap_or(""),
    );
    let user_message = ctx
        .get_messages()
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let has_tool_evidence = tool_calls_present
        || ctx
            .get_messages()
            .iter()
            .any(|m| m.role == MessageRole::Tool);

    let tool_calls_json: Vec<String> = response
        .message
        .tool_calls
        .as_ref()
        .map(|calls| calls.iter().map(|c| c.function.arguments.clone()).collect())
        .unwrap_or_default();
    let thinking_blocks: Vec<String> = response
        .message
        .reasoning_content
        .iter()
        .filter(|r| !r.trim().is_empty())
        .cloned()
        .collect();

    let evidence = crate::acceptance_gate::TurnEvidence {
        has_tool_evidence,
        user_requested_action: crate::acceptance_gate::user_requests_action(&user_message),
        user_message: &user_message,
    };
    let report = gate.evaluate_turn(
        &visible,
        &tool_calls_json,
        &thinking_blocks,
        !user_message.is_empty(),
        &evidence,
    );
    if let Some(advisory) = report.advisory_text() {
        let message = format!(
            "{}{advisory}\n\
             若上一条回复把尚未执行的动作说成已完成，请先真正执行该动作再汇报；\n\
             若确实无法执行，请明确说明无法执行的原因。",
            crate::acceptance_gate::ADVISORY_MESSAGE_PREFIX
        );
        // ★ Do not repeat an advisory the model already has.
        //
        // The check can fire on more than one turn of the same run: a
        // model that answers with a completion claim and no tool call
        // is nudged ("you have not yet started working") and the loop
        // runs again — and the claim is still unbacked, so the check
        // fires again. Injecting the same words twice buys nothing
        // (the model already has them in context) and costs context on
        // every such turn.
        //
        // The advisory quotes the user's message, so its text is stable
        // for a given (session, user message) pair — an exact match is
        // the right de-duplication key. The per-session cap is the
        // backstop for the case where the text differs.
        let existing = ctx.get_messages();
        let already_present = existing
            .iter()
            .any(|m| m.content.as_deref() == Some(message.as_str()));
        let advisory_count = existing
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .map(crate::acceptance_gate::is_advisory_message)
                    .unwrap_or(false)
            })
            .count();
        if already_present {
            tracing::debug!(
                turn = total_turns,
                "acceptance_gate: advisory already in context, not repeating it"
            );
            return;
        }
        if advisory_count >= crate::acceptance_gate::MAX_ADVISORIES_PER_SESSION {
            tracing::debug!(
                turn = total_turns,
                advisories = advisory_count,
                "acceptance_gate: per-session advisory cap reached, not adding another"
            );
            return;
        }
        tracing::warn!(
            turn = total_turns,
            advisory = %advisory,
            "acceptance_gate: advisory queued for the next turn"
        );
        // ★ Queue, do not inject here. This runs inside
        // `process_post_response`, which is called **before** the
        // assistant reply is committed (`prepare_assistant_turn`).
        // Injecting at this point put the reminder in front of the
        // reply it criticises, so "上一回复声称已完成" pointed at the
        // user's message. The loop flushes it right after the
        // assistant message lands — see `flush_acceptance_advisory`.
        gate.queue_advisory(message);
    }
}

/// Put a queued acceptance advisory into the context.
///
/// Called by the loop **after** `prepare_assistant_turn`, i.e. after the
/// assistant reply is in the context. A warning is a comment on that
/// reply, so it has to follow it; injected earlier it would describe a
/// message the model has not "said" yet in the transcript it reads.
pub(crate) fn flush_acceptance_advisory(
    ctx: &mut ContextManager,
    gate: &mut crate::acceptance_gate::AcceptanceGate,
) {
    if let Some(text) = gate.take_pending_advisory() {
        ctx.add_message(Message::system(text));
    }
}

/// .x : consolidate the session-end side effects
/// that fire at the end of every `run()` / `run_stream()` invocation:
/// 1. Build the session-end `Value` and fire `OnSessionEnd`.
/// 2. Optionally fire `OnSessionReset` when the last user message was
/// a `/reset` or `/new` command.
/// 3. Route `memory_on_session_end` (persist session-level
/// `FocusConclusion`, drain pending conclusions, call
/// `MemoryRouter::on_session_end`).
/// 4. Fire `OnSessionFinalize` (final turn count + cost snapshot).
/// 5. : persist accumulated token usage so `/insights` can chart
/// real numbers. Best-effort; failure logged but non-fatal.
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
    crate::memory_prefetch::memory_on_session_end(
        config,
        memory_router,
        focus_stack,
        messages,
        memory_callbacks,
    );
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

    //: persist the final accumulated token usage
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
        last_error: None,
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
/// Removed `RetryReasoning` (thinking-only inner-loop
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
    if *inner_empty < config.empty_content_max_retries {
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
    active_model: &str,
) -> Result<Option<Message>, AgentError> {
    ctx.add_message(Message::system(
        "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
         what was accomplished and any remaining tasks.",
    ));
    let extra_body = extra_body_for_api_mode(config, &config.api_mode);
    let summary_body = || {
        llm_provider.chat_completion(
            ctx.get_messages(),
            &[],
            config.max_tokens,
            config.temperature,
            Some(active_model),
            extra_body.as_ref(),
        )
    };
    match summary_body().await {
        Ok(response) => Ok(Some(response.message)),
        Err(e) => {
            // The dialog's own model failed (rate limit / quota / provider
            // error). Degrade to the configured default model once — the
            // summary should run on the user's chosen model first; the
            // default is only a fallback when that model is broken.
            let fallback = (config.model.as_str() != active_model).then(|| config.model.as_str());
            if let Some(fb) = fallback {
                match llm_provider
                    .chat_completion(
                        ctx.get_messages(),
                        &[],
                        config.max_tokens,
                        config.temperature,
                        Some(fb),
                        extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                    )
                    .await
                {
                    Ok(response) => return Ok(Some(response.message)),
                    Err(e2) => {
                        tracing::warn!(
                            error = %e2,
                            "handle_max_iterations: fallback summary call failed; finalizing without summary"
                        );
                        return Ok(None);
                    }
                }
            }
            tracing::warn!(
                error = %e,
                "handle_max_iterations: summary LLM call failed; finalizing without summary"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod backfill_tests {
    use super::*;
    use hermix_core::{MessageRole, ToolCall};

    fn assistant_with_calls(ids: &[&str]) -> Message {
        let mut m = Message::assistant("working");
        m.tool_calls = Some(
            ids.iter()
                .map(|id| ToolCall {
                    id: (*id).to_string(),
                    function: hermix_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: "{}".to_string(),
                    },
                })
                .collect(),
        );
        m
    }

    #[test]
    fn backfills_a_call_that_never_got_a_result() {
        let mut msgs = vec![
            Message::user("do it"),
            assistant_with_calls(&["call-1"]),
            // turn ended here — no tool result
        ];
        let n = backfill_unanswered_tool_results(&mut msgs);
        assert_eq!(n, 1, "one dangling call should be backfilled");
        let last = msgs.last().expect("backfilled message");
        assert_eq!(last.role, MessageRole::Tool);
        assert_eq!(last.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn leaves_answered_calls_untouched() {
        let mut msgs = vec![
            Message::user("do it"),
            assistant_with_calls(&["call-1"]),
            Message::tool_result("call-1", "done", Some("terminal")),
        ];
        let n = backfill_unanswered_tool_results(&mut msgs);
        assert_eq!(n, 0, "an answered call must not be duplicated");
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn backfills_only_the_dangling_call_in_a_mixed_batch() {
        let mut msgs = vec![
            Message::user("do it"),
            assistant_with_calls(&["call-1", "call-2", "call-3"]),
            Message::tool_result("call-2", "done", Some("terminal")),
        ];
        let n = backfill_unanswered_tool_results(&mut msgs);
        assert_eq!(n, 2, "call-1 and call-3 are unanswered");
        let ids: Vec<&str> = msgs
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert!(ids.contains(&"call-1"));
        assert!(ids.contains(&"call-2"));
        assert!(ids.contains(&"call-3"));
    }

    #[test]
    fn synthetic_result_is_adjacent_to_its_assistant_message() {
        // `enforce_strict_tool_pairing` drops tool messages whose pairing
        // was broken by an intervening non-tool message, so adjacency is
        // a hard requirement, not a nicety.
        let mut msgs = vec![
            Message::user("first"),
            assistant_with_calls(&["call-1"]),
            Message::user("intervening user turn"),
            assistant_with_calls(&["call-2"]),
        ];
        backfill_unanswered_tool_results(&mut msgs);
        for (i, m) in msgs.iter().enumerate() {
            if m.role == MessageRole::Tool {
                let prev = msgs[i - 1].role;
                assert_eq!(
                    prev,
                    MessageRole::Assistant,
                    "tool result at {i} must directly follow its assistant message"
                );
            }
        }
    }

    #[test]
    fn no_op_on_a_clean_history() {
        let mut msgs = vec![Message::user("hi"), Message::assistant("hello")];
        assert_eq!(backfill_unanswered_tool_results(&mut msgs), 0);
        assert_eq!(msgs.len(), 2);
    }
}
