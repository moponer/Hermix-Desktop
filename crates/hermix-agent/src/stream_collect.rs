//! Streaming-LLM-chunk assembly helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `assemble_stream_assistant_message` — pack content/reasoning/tool_calls
//!   into the final assistant message after a streaming collection.
//! - `apply_stream_collect_chunk` — apply one [`StreamChunk`] during
//!   [`collect_stream_llm_response`] — runs the think-block parser,
//!   the tool-call sanitizer, and forwards callbacks.
//! - `collect_stream_llm_response` — open a streaming completion,
//!   iterate over chunks, and assemble the final [`LlmResponse`].
//!   Supports graceful interrupt via [`InterruptController`] and a
//!   silence-watchdog that emits activity-status events when the
//!   stream stalls.
//! - `StreamCollectOutcome` — result enum (Complete / Interrupted).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::time::sleep;

use hermix_core::tool_call_sanitizer::ToolCallSanitizer;
use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall, ToolSchema, UsageStats};

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::agent_loop::TurnRuntimeRoute;
use crate::context::ContextManager;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::message_helpers::{emit_status, extra_body_for_api_mode};
use crate::oauth_refresh::OAuthRefresh;
use crate::prompt_assembly::{extract_provider_and_model, messages_for_api_call};
use crate::python_alignment::ThinkBlockParser;
use crate::runtime_provider::{build_runtime_provider, credential_pool_for_route};

/// Result of collecting one streaming completion.
#[derive(Debug)]
pub(crate) enum StreamCollectOutcome {
    Complete(LlmResponse),
    Interrupted(LlmResponse),
}

/// Pack the streamed content/reasoning/tool_calls into the final assistant
/// message.
///
/// Always sets a `content` string when `tool_calls` are present, even when
/// the sanitizer has stripped the raw XML to an empty string. Some API
/// providers (MiniMax M3) reject assistant messages whose `content` field
/// is `null` or missing when they carry `tool_calls`.
pub fn assemble_stream_assistant_message(
    content: &str,
    reasoning_content: &str,
    tool_calls: &[ToolCall],
) -> Message {
    if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
        let mut m = Message::assistant(content.to_string());
        if !reasoning_content.is_empty() {
            m.reasoning_content = Some(reasoning_content.to_string());
        }
        m
    } else {
        // Always set a `content` string when tool_calls are
        // present, even when the sanitizer has stripped the raw
        // XML to an empty string.  Some API providers (MiniMax
        // M3) reject assistant messages whose `content` field is
        // `null` or missing when they carry `tool_calls`.
        let content_opt = Some(if content.is_empty() {
            String::new()
        } else {
            content.to_string()
        });
        let mut m = Message::assistant_with_tool_calls(content_opt, tool_calls.to_vec());
        if !reasoning_content.is_empty() {
            m.reasoning_content = Some(reasoning_content.to_string());
        }
        m
    }
}

/// Apply one streaming [`StreamChunk`] during `collect_stream_llm_response`.
///
/// `think_parser` is a stateful splitter that pulls `<think>...</think>` text
/// out of the visible content stream in real time and routes it to
/// `reasoning_content` instead. Without this, providers that embed their
/// reasoning in `delta.content` (MiniMax M2.7, Qwen, DeepSeek-R1) would
/// leak the whole thinking monologue into the user-visible reply.
///
/// `tool_call_sanitizer` is a stateful redactor that pulls raw
/// `<minimax:tool_call>…</minimax:tool_call>` /
/// `<invoke name="…">…</invoke>` blocks out of the *clean* (post-think)
/// text. Without it, providers that emit tool calls as raw XML inside
/// `delta.content` (MiniMax, older Hermes fine-tunes) would leak the
/// whole XML envelope to the user. See
/// `hermix_core::tool_call_sanitizer` for the full state machine.
#[allow(clippy::too_many_arguments)]
pub fn apply_stream_collect_chunk(
    callbacks: &AgentCallbacks,
    chunk: StreamChunk,
    content: &mut String,
    raw_content: &mut String,
    reasoning_content: &mut String,
    think_parser: &mut ThinkBlockParser,
    tool_call_sanitizer: &mut ToolCallSanitizer,
    tool_calls: &mut Vec<ToolCall>,
    last_usage: &mut Option<UsageStats>,
    finish_reason: &mut Option<String>,
    on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
) {
    let mut chunk = chunk;
    // 2026-07-14 (Frontend Audit, "思考被切成两段"): track whether
    // the think-block parser above already emitted thinking for
    // THIS chunk. Some providers (MiniMax M2.7 with the
    // `extra.thinking` adapter, hybrid transports that re-wrap a
    // streaming provider) populate BOTH `delta.content` with a
    // `<think>...</think>` span AND `delta.extra.thinking` for the
    // same logical content. Without this guard the
    // `callbacks.on_thinking` fires twice — the chat UI accumulates
    // the same sentence into `unified_steps` twice, rendering as
    // TWO "已深度思考" blocks for what the model said once.
    //
    // Strict priority order: parser-extracted `think_text`
    // (canonical — same path that the persisted `reasoning_content`
    // is built from) wins over `delta.extra.thinking`. We only
    // consult the alternative path when the parser had nothing
    // to emit this chunk.
    let mut thinking_emitted_this_chunk = false;
    if let Some(ref mut delta) = chunk.delta {
        if let Some(ref mut text) = delta.content {
            let (clean_text, think_text) = think_parser.feed(text);
            // Keep a copy of the pre-sanitizer text so we can
            // later parse it for tool calls hidden inside
            // `<minimax:tool_call>` blocks (MiniMax-M2 / M2.5).
            raw_content.push_str(&clean_text);
            // Second pass: scrub raw tool-call XML out of the
            // *clean* text. We do this BEFORE both
            //   (a) the local `content` accumulator (so the
            //       persisted assistant message is clean on
            //       reload) and
            //   (b) the chunk we forward to the WS layer (so
            //       the live stream never shows leaked XML).
            let visible_text = tool_call_sanitizer.feed(&clean_text);
            if !visible_text.is_empty() {
                content.push_str(&visible_text);
                if let Some(ref cb) = callbacks.on_stream_delta {
                    cb(&visible_text);
                }
            }
            // If the sanitizer ate the whole clean text,
            // rewrite `delta.content` to the (possibly empty)
            // visible text so the downstream `on_chunk` wrapper
            // doesn't forward the raw XML. This is the part
            // that fixes the user-visible leak.
            *text = visible_text;
            if !think_text.is_empty() {
                reasoning_content.push_str(&think_text);
                if let Some(ref cb) = callbacks.on_thinking {
                    cb(&think_text);
                }
                thinking_emitted_this_chunk = true;
            }
        }
        if !thinking_emitted_this_chunk {
            if let Some(ref extra) = delta.extra {
                if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                    if !thinking.is_empty() {
                        reasoning_content.push_str(thinking);
                        if let Some(ref cb) = callbacks.on_thinking {
                            cb(thinking);
                        }
                    }
                }
            }
        }
        if let Some(ref tc_deltas) = delta.tool_calls {
            for tcd in tc_deltas {
                let idx = tcd.index as usize;
                while tool_calls.len() <= idx {
                    tool_calls.push(ToolCall {
                        id: String::new(),
                        function: hermix_core::FunctionCall {
                            name: String::new(),
                            arguments: String::new(),
                        },
                    });
                }
                if let Some(ref id) = tcd.id {
                    tool_calls[idx].id = id.clone();
                }
                if let Some(ref fc) = tcd.function {
                    if let Some(ref name) = fc.name {
                        tool_calls[idx].function.name = name.clone();
                    }
                    if let Some(ref args) = fc.arguments {
                        tool_calls[idx].function.arguments.push_str(args);
                    }
                }
            }
        }
    }

    if let Some(ref usage) = chunk.usage {
        *last_usage = Some(usage.clone());
    }
    if let Some(ref fr) = chunk.finish_reason {
        *finish_reason = Some(fr.clone());
    }

    on_chunk(chunk);
}

/// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
///
/// Opens a streaming completion, iterates over chunks, and assembles the final
/// response. Supports graceful interrupt and a silence-watchdog that emits
/// activity-status events when the stream stalls.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "collect_stream_llm_response",
    level = "info",
    skip_all,
    fields(
        active_model = %active_model,
        tool_count = tool_schemas.len(),
        route_label = route.and_then(|r| r.route_label.as_deref()).unwrap_or(""),
        chunks_emitted = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
        finish_reason = tracing::field::Empty,
        outcome = tracing::field::Empty,
    )
)]
pub(crate) async fn collect_stream_llm_response(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    callbacks: &AgentCallbacks,
    llm_provider: &Arc<dyn LlmProvider>,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    interrupt: &InterruptController,
    ctx: &ContextManager,
    tool_schemas: &[ToolSchema],
    route: Option<&TurnRuntimeRoute>,
    active_model: &str,
    on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
) -> Result<StreamCollectOutcome, AgentError> {
    // 2026-07-18 (Round 29 follow-up): outer hard timeout. The inner
    // silence_watch only emits "activity" notices — it does not abort
    // the stream — so a provider that opens the SSE / chunked
    // connection and then never sends a chunk (network hang, API
    // outage, silently dropped TCP) used to pin the agent loop and
    // the UI at "正在思考" forever. We now wrap the entire inner
    // collection future in `tokio::time::timeout` and, on expiry,
    // assemble an empty partial reply with `finish_reason =
    // "timeout"`. `decide_empty_response` then routes the empty
    // turn through `decide_empty_response` exactly like any other
    // thinking-only reply, so a stuck provider degrades into a
    // visible post-tool nudge (or finalizes if all nudges are
    // exhausted) instead of hanging.
    //
    // `stream_overall_timeout_secs == 0` opts out of the watchdog
    // (legacy behaviour).
    let overall_secs = config.stream_overall_timeout_secs;
    let inner_fut = collect_stream_inner(
        config,
        oauth,
        callbacks,
        llm_provider,
        primary_credential_pool,
        interrupt,
        ctx,
        tool_schemas,
        route,
        active_model,
        on_chunk,
    );
    if overall_secs == 0 {
        return inner_fut.await;
    }
    let dur = std::time::Duration::from_secs(overall_secs);
    match tokio::time::timeout(dur, inner_fut).await {
        Ok(outcome) => outcome,
        Err(_elapsed) => {
            tracing::warn!(
                active_model = %active_model,
                timeout_secs = overall_secs,
                "streaming LLM completion hit hard timeout — provider not responding, degrading to empty partial reply"
            );
            emit_status(
                config,
                callbacks,
                "activity",
                &format!(
                    "LLM stream timed out after {overall_secs}s — provider not responding. \
                     The agent will finalize this turn with an empty reply; retry or check the \
                     upstream provider if this persists."
                ),
            );
            // Partial state is dropped when the inner future is cancelled
            // by `tokio::time::timeout`. That's acceptable here: any
            // content / reasoning / tool_calls that did make it through
            // arrived via a chunk *before* the deadline, so the agent
            // loop would have already received them via `on_chunk`.
            // Anything *after* the deadline is, by definition, not part
            // of this turn. The empty reply + `finish_reason = "timeout"`
            // is the single source of truth for "this turn was cut
            // short by the watchdog".
            let message = assemble_stream_assistant_message("", "", &[]);
            Ok(StreamCollectOutcome::Complete(LlmResponse {
                message,
                usage: None,
                model: active_model.to_string(),
                finish_reason: Some("timeout".to_string()),
            }))
        }
    }
}

/// Inner streaming-collection body — pre-hard-timeout implementation,
/// called by [`collect_stream_llm_response`] which adds the outer
/// watchdog. Kept as a separate function so the inner future can be
/// `tokio::time::timeout`-wrapped without mutating any of the
/// per-chunk mutable state.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn collect_stream_inner(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    callbacks: &AgentCallbacks,
    llm_provider: &Arc<dyn LlmProvider>,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    interrupt: &InterruptController,
    ctx: &ContextManager,
    tool_schemas: &[ToolSchema],
    route: Option<&TurnRuntimeRoute>,
    active_model: &str,
    on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
) -> Result<StreamCollectOutcome, AgentError> {
    let api_messages = messages_for_api_call(config, ctx);
    let default_extra_body = extra_body_for_api_mode(config, &config.api_mode);
    let (_, active_request_model) = extract_provider_and_model(config, active_model);
    let (_, primary_request_model) =
        extract_provider_and_model(config, config.model.as_str());
    let mut stream = if let Some(rt) = route {
        let (provider_name, model_name) = extract_provider_and_model(config, active_model);
        let mode = rt.api_mode.as_ref().unwrap_or(&config.api_mode);
        let extra_body_for_call = extra_body_for_api_mode(config, mode);
        let pool = credential_pool_for_route(primary_credential_pool, rt);
        match build_runtime_provider(
            config,
            oauth,
            rt.provider.as_deref().unwrap_or(provider_name.as_str()),
            model_name,
            rt.base_url.as_deref(),
            rt.api_key_env.as_deref(),
            None,
            Some(mode),
            pool,
        ) {
            Ok(provider) => provider.chat_completion_stream(
                &api_messages,
                tool_schemas,
                config.max_tokens,
                config.temperature,
                Some(model_name),
                extra_body_for_call.as_ref(),
            ),
            Err(e) => {
                tracing::warn!(
                    "Runtime route unavailable (reason={:?}) for stream, falling back to primary runtime: {}",
                    rt.routing_reason,
                    e
                );
                llm_provider.chat_completion_stream(
                    &api_messages,
                    tool_schemas,
                    config.max_tokens,
                    config.temperature,
                    Some(primary_request_model),
                    default_extra_body.as_ref(),
                )
            }
        }
    } else {
        llm_provider.chat_completion_stream(
            &api_messages,
            tool_schemas,
            config.max_tokens,
            config.temperature,
            Some(active_request_model),
            default_extra_body.as_ref(),
        )
    };

    let mut content = String::new();
    let mut raw_content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut last_usage: Option<UsageStats> = None;
    let mut finish_reason: Option<String> = None;
    let mut think_parser = ThinkBlockParser::new();
    let mut tool_call_sanitizer = ToolCallSanitizer::new();

    let activity_secs = config.stream_activity_interval_secs;

    if activity_secs == 0 {
        while let Some(chunk_result) = stream.next().await {
            if interrupt.take_interrupt_graceful().is_some() {
                let (flush_content, flush_reasoning) = think_parser.flush();
                if !flush_content.is_empty() {
                    content.push_str(&flush_content);
                }
                if !flush_reasoning.is_empty() {
                    reasoning_content.push_str(&flush_reasoning);
                }
                tool_call_sanitizer.flush();
                let message = assemble_stream_assistant_message(
                    &content,
                    &reasoning_content,
                    &tool_calls,
                );
                return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                    message,
                    usage: last_usage.clone(),
                    model: active_model.to_string(),
                    finish_reason: Some("interrupted".to_string()),
                }));
            }
            let chunk = chunk_result?;
            apply_stream_collect_chunk(
                callbacks,
                chunk,
                &mut content,
                &mut raw_content,
                &mut reasoning_content,
                &mut think_parser,
                &mut tool_call_sanitizer,
                &mut tool_calls,
                &mut last_usage,
                &mut finish_reason,
                on_chunk,
            );
        }
    } else {
        const IDLE_ARM_MAX: Duration = Duration::from_secs(86_400);
        let first_dur = Duration::from_secs(activity_secs);
        let stall_secs = config.stream_activity_stall_secs;
        let stall_dur = Duration::from_secs(stall_secs);

        'silence_watch: loop {
            let mut idle = Box::pin(sleep(first_dur));
            let mut warm_notice_sent = false;
            let mut stall_notice_sent = false;
            loop {
                tokio::select! {
                    biased;
                    chunk_opt = stream.next() => {
                        match chunk_opt {
                            None => break 'silence_watch,
                            Some(chunk_result) => {
                                if interrupt.take_interrupt_graceful().is_some() {
                                    let message = assemble_stream_assistant_message(
                                        &content,
                                        &reasoning_content,
                                        &tool_calls,
                                    );
                                    return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                        message,
                                        usage: last_usage.clone(),
                                        model: active_model.to_string(),
                                        finish_reason: Some("interrupted".to_string()),
                                    }));
                                }
                                let chunk = chunk_result?;
                                apply_stream_collect_chunk(
                                    callbacks,
                                    chunk,
                                    &mut content,
                                    &mut raw_content,
                                    &mut reasoning_content,
                                    &mut think_parser,
                                    &mut tool_call_sanitizer,
                                    &mut tool_calls,
                                    &mut last_usage,
                                    &mut finish_reason,
                                    on_chunk,
                                );
                                continue 'silence_watch;
                            }
                        }
                    }
                    _ = idle.as_mut() => {
                        if !warm_notice_sent {
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                "Still waiting for the model…",
                            );
                            warm_notice_sent = true;
                            if stall_secs > 0 {
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + stall_dur);
                            } else {
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                            }
                        } else if stall_secs > 0 && !stall_notice_sent {
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                "Still no model output — the stream may be stuck.",
                            );
                            stall_notice_sent = true;
                            idle.as_mut()
                                .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                        } else {
                            idle.as_mut()
                                .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                        }
                    }
                }
            }
        }
    }

    let has_truncated_tool_args = tool_calls.iter().any(|tc| {
        let trimmed = tc.function.arguments.trim();
        !trimmed.is_empty() && serde_json::from_str::<serde_json::Value>(trimmed).is_err()
    });
    if has_truncated_tool_args {
        finish_reason = Some("length".to_string());
    }

    let (flush_content, flush_reasoning) = think_parser.flush();
    if !flush_content.is_empty() {
        content.push_str(&flush_content);
        raw_content.push_str(&flush_content);
    }
    if !flush_reasoning.is_empty() {
        reasoning_content.push_str(&flush_reasoning);
    }
    tool_call_sanitizer.flush();

    // 2026-07-17: last-line defence for models that ignore the system
    // prompt and emit an unwrapped reasoning monologue as visible content.
    // If the whole reply reads like internal analysis ("I think...",
    // "trying to reconstruct...", etc.) and never used a ` <think` tag,
    // route it to `reasoning_content` so the UI renders it as a collapsed
    // thinking block instead of flooding the chat.
    if !content.is_empty()
        && reasoning_content.is_empty()
        && crate::python_alignment::looks_like_untagged_reasoning(&content)
    {
        tracing::warn!(
            content_len = content.len(),
            "untagged reasoning monologue detected; routing to reasoning_content"
        );
        std::mem::swap(&mut content, &mut reasoning_content);
    }

    if tool_calls.is_empty() {
        let parsed = hermix_core::tool_call_parser::parse_tool_calls(&raw_content)
            .unwrap_or_default();
        if !parsed.is_empty() {
            tracing::info!(
                parsed_count = parsed.len(),
                raw_len = raw_content.len(),
                "Parsed tool calls from raw XML text (MiniMax/M2.x compat)"
            );
            tool_calls = parsed;
        }
    }

    // 2026-07-08: filter out tool calls with empty function names.
    // MiniMax M2.7 streaming sometimes creates placeholder tool call
    // entries (via the index-based delta assembly above) whose `name`
    // field never arrives — the name delta was either lost in transit
    // or the model emitted the tool call as raw XML that the sanitizer
    // already stripped. Without this filter, `process_post_response`
    // sees `has_tool_calls = true` (vec is non-empty) and returns
    // `Continue`, but then `run_stream` filters them out at line ~1315
    // and falls into the `decide_empty_response` path — which in
    // autonomous_mode fires a nudge that re-drives the LLM to re-run
    // the same tools. Filtering here ensures `process_post_response`
    // sees the correct state (no valid tool calls → `Finalize`).
    let empty_name_count = tool_calls.iter().filter(|tc| tc.function.name.is_empty()).count();
    if empty_name_count > 0 {
        tracing::info!(
            total = tool_calls.len(),
            empty_names = empty_name_count,
            "Filtering out tool calls with empty function names (streaming placeholder entries)"
        );
        tool_calls.retain(|tc| !tc.function.name.is_empty());
    }

    let message =
        assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);

    Ok(StreamCollectOutcome::Complete(LlmResponse {
        message,
        usage: last_usage,
        model: active_model.to_string(),
        finish_reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupt::InterruptController;
    use crate::oauth_refresh::OAuthRefresh;
    use async_stream::stream;
    use futures::stream::BoxStream;
    use hermix_core::{AgentError, LlmProvider, LlmResponse, StreamDelta};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 2026-07-14 (Frontend Audit, "思考被切成两段"): when a single
    /// LLM chunk carries BOTH a `<think>...</think>` span in
    /// `delta.content` AND a `delta.extra.thinking` value, the
    /// chat UI used to render the same sentence as TWO "已深度思考"
    /// blocks (one per firing of `callbacks.on_thinking`). The fix
    /// gives the parser-extracted `think_text` priority and skips
    /// the `extra.thinking` fallback path entirely for that chunk.
    #[test]
    fn apply_stream_collect_chunk_dedupes_double_thinking_emission() {
        let on_thinking_count = Arc::new(Mutex::new(0usize));
        let collected_thinking = Arc::new(Mutex::new(String::new()));
        let count_for_cb = on_thinking_count.clone();
        let collected_for_cb = collected_thinking.clone();
        let callbacks = AgentCallbacks {
            on_thinking: Some(Box::new(move |text: &str| {
                *count_for_cb.lock().unwrap() += 1;
                collected_for_cb.lock().unwrap().push_str(text);
            })),
            ..Default::default()
        };

        let chunk = StreamChunk {
            delta: Some(StreamDelta {
                content: Some("<think>thinking part 1</think>".to_string()),
                tool_calls: None,
                extra: Some(serde_json::json!({
                    "thinking": "thinking part 1",
                })),
            }),
            finish_reason: None,
            usage: None,
        };

        let mut content = String::new();
        let mut raw_content = String::new();
        let mut reasoning_content = String::new();
        let mut think_parser = ThinkBlockParser::new();
        let mut tool_call_sanitizer = ToolCallSanitizer::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage: Option<UsageStats> = None;
        let mut finish_reason: Option<String> = None;
        apply_stream_collect_chunk(
            &callbacks,
            chunk,
            &mut content,
            &mut raw_content,
            &mut reasoning_content,
            &mut think_parser,
            &mut tool_call_sanitizer,
            &mut tool_calls,
            &mut last_usage,
            &mut finish_reason,
            &|_ch| {},
        );

        let count = *on_thinking_count.lock().unwrap();
        let collected = collected_thinking.lock().unwrap().clone();
        // The fix: exactly one emission per chunk, even when both
        // paths had content. The duplicated sentence would have
        // produced count == 2 and `collected == "thinking part 1thinking part 1"`.
        assert_eq!(
            count, 1,
            "expected exactly one on_thinking call per chunk, got {count} (collected={collected:?})",
        );
        assert_eq!(collected, "thinking part 1");
        // Reasoning accumulator must only contain the parser's output,
        // not a duplicate concatenation of both paths.
        assert_eq!(reasoning_content, "thinking part 1");
    }

    /// When the parser had nothing to emit but `delta.extra.thinking`
    /// carries a value (the pure non-embedded path), the fallback
    /// must still fire — otherwise we silently drop live reasoning
    /// for providers that only use the `extra.thinking` channel.
    #[test]
    fn apply_stream_collect_chunk_uses_extra_thinking_when_parser_empty() {
        let on_thinking_count = Arc::new(Mutex::new(0usize));
        let collected_thinking = Arc::new(Mutex::new(String::new()));
        let count_for_cb = on_thinking_count.clone();
        let collected_for_cb = collected_thinking.clone();
        let callbacks = AgentCallbacks {
            on_thinking: Some(Box::new(move |text: &str| {
                *count_for_cb.lock().unwrap() += 1;
                collected_for_cb.lock().unwrap().push_str(text);
            })),
            ..Default::default()
        };

        let chunk = StreamChunk {
            delta: Some(StreamDelta {
                content: Some("visible reply, no think tags".to_string()),
                tool_calls: None,
                extra: Some(serde_json::json!({
                    "thinking": "extra-only reasoning",
                })),
            }),
            finish_reason: None,
            usage: None,
        };

        let mut content = String::new();
        let mut raw_content = String::new();
        let mut reasoning_content = String::new();
        let mut think_parser = ThinkBlockParser::new();
        let mut tool_call_sanitizer = ToolCallSanitizer::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage: Option<UsageStats> = None;
        let mut finish_reason: Option<String> = None;
        apply_stream_collect_chunk(
            &callbacks,
            chunk,
            &mut content,
            &mut raw_content,
            &mut reasoning_content,
            &mut think_parser,
            &mut tool_call_sanitizer,
            &mut tool_calls,
            &mut last_usage,
            &mut finish_reason,
            &|_ch| {},
        );

        let count = *on_thinking_count.lock().unwrap();
        let collected = collected_thinking.lock().unwrap().clone();
        assert_eq!(
            count, 1,
            "expected one fallback emission from extra.thinking, got {count}",
        );
        assert_eq!(collected, "extra-only reasoning");
        assert_eq!(reasoning_content, "extra-only reasoning");
    }

    // -----------------------------------------------------------------------
    // 2026-07-18: hard outer timeout tests for `collect_stream_llm_response`.
    //
    // Symptom (Round 29): provider opens the SSE connection and then
    // never sends a chunk (network hang, API outage, silently dropped
    // TCP). Without the outer timeout the inner `silence_watch` only
    // emits activity notices — it does not abort — so the agent loop
    // and the UI both pinned at "正在思考" forever. Fix: wrap the
    // collection future in `tokio::time::timeout` and, on expiry,
    // assemble an empty partial reply with `finish_reason = "timeout"`
    // so `decide_empty_response` can route through the post-tool
    // nudge or finalize.
    // -----------------------------------------------------------------------

    /// Mock provider whose `chat_completion_stream` blocks indefinitely
    /// — never yields a chunk, never returns `None`. Models the "TCP
    /// open but no data" failure mode that previously hung the agent
    /// loop.
    struct HangingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for HangingProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            // Never resolves — same as the hanging stream.
            std::future::pending::<()>().await;
            unreachable!()
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            // Empty stream that hangs forever — the future yields
            // nothing and never returns. The outer
            // `tokio::time::timeout` is what bounds the call.
            Box::pin(stream! {
                // Hanging stream — future resolves to `None` only after
                // a `Pending` that never wakes. The outer
                // `tokio::time::timeout` is what bounds the call; the
                // `yield` is dead code that just gives the stream a
                // concrete `Stream<Item = Result<StreamChunk, _>>`
                // type so the trait cast works.
                let _ = std::future::pending::<Result<StreamChunk, AgentError>>().await;
                #[allow(unreachable_code)]
                {
                    yield Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some(String::from("unreachable")),
                        usage: None,
                    });
                }
            })
        }
    }

    /// Mock provider that yields a single text chunk then EOF. Models
    /// the happy path so we can assert the watchdog doesn't fire when
    /// the provider actually responds.
    struct QuickProvider;

    #[async_trait::async_trait]
    impl LlmProvider for QuickProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: hermix_core::Message::assistant(String::from("noop")),
                usage: None,
                model: String::from("dummy"),
                finish_reason: Some(String::from("stop")),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(stream! {
                yield Ok(StreamChunk {
                    delta: Some(StreamDelta {
                        content: Some(String::from("hi")),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
                yield Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some(String::from("stop")),
                    usage: None,
                });
            })
        }
    }

    /// Drive `collect_stream_llm_response` with a fully-defaulted
    /// scaffold, mutating only the LLM provider and the
    /// `stream_overall_timeout_secs` config. Returns whatever
    /// `StreamCollectOutcome` the wrapper surfaces.
    async fn drive_collect(
        provider: Arc<dyn LlmProvider>,
        stream_overall_timeout_secs: u64,
    ) -> Result<StreamCollectOutcome, AgentError> {
        let mut config = AgentConfig::default();
        // Silence the inner activity watchdog so it doesn't bias
        // timing measurements. The hard timeout is what we're
        // testing here.
        config.stream_activity_interval_secs = 0;
        config.stream_overall_timeout_secs = stream_overall_timeout_secs;

        let oauth = OAuthRefresh::new();
        let interrupt = InterruptController::new();
        let ctx = crate::context::ContextManager::new(2048);

        let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let chunks_ref = chunks.clone();
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> = Box::new(move |c: StreamChunk| {
            if let Some(delta) = c.delta {
                if let Some(content) = delta.content {
                    chunks_ref.lock().unwrap().push(content);
                }
            }
        });

        let outcome = crate::stream_collect::collect_stream_llm_response(
            &config,
            &oauth,
            &AgentCallbacks::default(),
            &provider,
            None,
            &interrupt,
            &ctx,
            &[],
            None,
            "test-model",
            &*on_chunk,
        )
        .await?;
        // Suppress the unused-variable warning when the normal
        // path is exercised.
        let _ = chunks;
        Ok(outcome)
    }

    /// Provider hangs forever → outer watchdog fires after
    /// `stream_overall_timeout_secs` (1 s) and the wrapper returns
    /// `StreamCollectOutcome::Complete { finish_reason: "timeout" }`
    /// with an empty assistant message. Without the watchdog this
    /// test would never return — the agent loop would stay pinned
    /// at "正在思考" forever (the Round 29 bug).
    #[tokio::test]
    async fn hard_timeout_fires_when_provider_never_chunks() {
        // Test-level wall-clock cap: 10 s is generous compared to the
        // 1 s inner timeout and would expose a regression where the
        // watchdog is no longer wired.
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            drive_collect(Arc::new(HangingProvider), 1),
        )
        .await
        .expect("collect_stream_llm_response hung past 10s — hard timeout not wired")
        .expect("collect_stream_llm_response returned Err");

        match outcome {
            StreamCollectOutcome::Complete(resp) => {
                assert_eq!(
                    resp.finish_reason.as_deref(),
                    Some("timeout"),
                    "expected finish_reason=\"timeout\" on hard timeout, got {:?}",
                    resp.finish_reason
                );
                assert!(
                    resp.message.content.as_deref().unwrap_or("").is_empty(),
                    "expected empty assistant content on hard timeout, got {:?}",
                    resp.message.content
                );
                assert!(
                    resp.message
                        .tool_calls
                        .as_ref()
                        .map(|v| v.is_empty())
                        .unwrap_or(true),
                    "expected no tool_calls on hard timeout, got {:?}",
                    resp.message.tool_calls
                );
            }
            other => panic!("expected StreamCollectOutcome::Complete, got {:?}", other),
        }
    }

    /// Provider replies normally → outer watchdog does NOT fire;
    /// the collector returns whatever the inner stream assembled
    /// (here: finish_reason = "stop", content = "hi"). This guards
    /// against an over-eager watchdog that would swallow healthy
    /// long-running streams.
    #[tokio::test]
    async fn hard_timeout_does_not_fire_when_provider_chunks_normally() {
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            drive_collect(Arc::new(QuickProvider), 60),
        )
        .await
        .expect("collect_stream_llm_response hung past 10s — false-positive timeout?")
        .expect("collect_stream_llm_response returned Err");

        match outcome {
            StreamCollectOutcome::Complete(resp) => {
                assert_eq!(
                    resp.finish_reason.as_deref(),
                    Some("stop"),
                    "expected finish_reason=\"stop\" on normal completion, got {:?}",
                    resp.finish_reason
                );
                assert_eq!(
                    resp.message.content.as_deref(),
                    Some("hi"),
                    "expected content=\"hi\", got {:?}",
                    resp.message.content
                );
            }
            other => panic!("expected StreamCollectOutcome::Complete, got {:?}", other),
        }
    }
}