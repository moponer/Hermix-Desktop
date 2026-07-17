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
    use hermix_core::StreamDelta;
    use std::sync::{Arc, Mutex};

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
}