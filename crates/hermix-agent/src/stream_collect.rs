//! Streaming-LLM-chunk assembly helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
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
    /// `Interrupted` carries the captured backend error (if any). For a
    /// user-initiated interrupt this is `None`; for a mid-stream
    /// transport/provider failure (`finish_reason == "stream_error"`)
    /// it holds the raw error text so `run_loop` can thread it into
    /// `AgentResult::last_error` for the frontend error card.
    Interrupted(LlmResponse, Option<String>),
}

/// Pack the streamed content/reasoning/tool_calls into the final assistant
/// message.
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
        //) reject assistant messages whose `content` field is
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
/// `think_parser` is a stateful splitter that pulls `<think>...</think>` text
/// out of the visible content stream in real time and routes it to
/// `reasoning_content` instead. Without this, providers that embed their
/// reasoning in `delta.content` (MiniMax M2.7, Qwen, DeepSeek-R1) would
/// leak the whole thinking monologue into the user-visible reply.
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
    was_untagged_reasoning_detected: &mut bool,
    on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
) {
    let mut chunk = chunk;
    //track whether
    // the think-block parser above already emitted thinking for
    // THIS chunk. Some providers (MiniMax M2.7 with the
    // `extra.thinking` adapter, hybrid transports that re-wrap a
    // streaming provider) populate BOTH `delta.content` with a
    // `<think>...</think>` span AND `delta.extra.thinking` for the
    // same logical content. Without this guard the
    // `callbacks.on_thinking` fires twice — the chat UI accumulates
    // the same sentence into `unified_steps` twice, rendering as
    // TWO "已深度思考" blocks for what the model said once.
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
            //`<minimax:tool_call>` blocks (MiniMax-M2 / ).
            raw_content.push_str(&clean_text);
            // Second pass: scrub raw tool-call XML out of the
            // *clean* text. We do this BEFORE both
            // (a) the local `content` accumulator (so the
            // persisted assistant message is clean on
            // reload) and
            // (b) the chunk we forward to the WS layer (so
            // the live stream never shows leaked XML).
            let visible_text = tool_call_sanitizer.feed(&clean_text);
            if !visible_text.is_empty() {
                // ── Streaming untagged-reasoning detection ──────────
                // Some models (MiniMax M3 in certain modes, open-weight
                // fine-tunes) put their reasoning monologue directly in
                // `delta.content` WITHOUT `<think>` tags and WITHOUT
                // `delta.reasoning_content`. The flush-time
                // `looks_like_untagged_reasoning` check catches this for
                // the persisted message, but during streaming the text
                // has already been forwarded as `StreamEvent::Text` →
                // the frontend rendered it as body text (visible, not
                // folded) instead of a reasoning block.
                //
                // Fix: check on EVERY chunk while reasoning_content is
                // still empty (no <think> tags or delta.extra.thinking
                // yet). Once the accumulated `content` matches the
                // untagged-reasoning heuristic, move ALL accumulated
                // content to reasoning and set the
                // `was_untagged_reasoning_detected` flag so subsequent
                // chunks are also routed to reasoning.
                //
                // IMPORTANT: the flag is the ONLY way to enter
                // continuation mode. The previous heuristic
                // (`!reasoning_content.is_empty() && content.is_empty()
                // && !think_parser.is_in_think()`) was fundamentally
                // broken — it also matched the state AFTER `</think>`
                // (reasoning_content non-empty from the think block,
                // content empty because body text hasn't started yet,
                // think_parser not in think mode), causing ALL body text
                // to be swallowed into the reasoning fold.
                if !*was_untagged_reasoning_detected
                    && reasoning_content.is_empty()
                    && !thinking_emitted_this_chunk
                {
                    // If tool_calls deltas have already arrived, the
                    // model is in tool-dispatch mode — the short text
                    // before the tool call is the model's visible
                    // output (e.g. "让我查看一下目录"), NOT reasoning.
                    // Skip the untagged-reasoning heuristic entirely;
                    // swallowing it into the reasoning fold causes
                    // the user to see only a thinking block with no
                    // body text followed by a tool card.
                    let has_tool_call_deltas = !tool_calls.is_empty();
                    // Tentatively push to content so the heuristic
                    // can evaluate the full accumulated text.
                    content.push_str(&visible_text);
                    if let Some(ref cb) = callbacks.on_stream_delta {
                        cb(&visible_text);
                    }
                    if !has_tool_call_deltas
                        && crate::python_alignment::looks_like_untagged_reasoning(content)
                    {
                        // Detected! Move ALL accumulated content to
                        // reasoning. Previous chunks were already
                        // forwarded as `StreamEvent::Text` — the
                        // frontend's `onReasoningChunk` handler will
                        // retroactively convert those text steps to
                        // reasoning steps (see App.tsx).
                        let moved = std::mem::take(content);
                        reasoning_content.push_str(&moved);
                        // Set the flag so subsequent chunks are routed
                        // to reasoning via the continuation branch.
                        *was_untagged_reasoning_detected = true;
                        // Only call on_thinking for THIS chunk's text
                        // (previous chunks' text was already sent as
                        // StreamEvent::Text; calling on_thinking with
                        // the full `moved` would duplicate it).
                        if let Some(ref cb) = callbacks.on_thinking {
                            cb(&visible_text);
                        }
                        thinking_emitted_this_chunk = true;
                        // Don't forward as text — clear delta.content.
                        *text = String::new();
                    } else {
                        // Not reasoning — keep as visible text.
                        *text = visible_text;
                    }
                } else if *was_untagged_reasoning_detected {
                    // Continuation: untagged-reasoning was detected in
                    // a previous chunk — route directly to reasoning.
                    reasoning_content.push_str(&visible_text);
                    if let Some(ref cb) = callbacks.on_thinking {
                        cb(&visible_text);
                    }
                    thinking_emitted_this_chunk = true;
                    *text = String::new();
                } else {
                    // Normal text (reasoning_content non-empty from
                    // <think> tags or delta.extra.thinking — this is
                    // the model's actual reply body).
                    content.push_str(&visible_text);
                    if let Some(ref cb) = callbacks.on_stream_delta {
                        cb(&visible_text);
                    }
                    *text = visible_text;
                }
            } else {
                // visible_text is empty — still update *text so
                // downstream on_chunk sees the sanitized (empty) text.
                *text = visible_text;
            }
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
    //( follow-up): outer hard timeout. The inner
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
    // 分段探针：把「哪一段字节在变」打进日志 —— 缓存失效的起点就在那一段。
    crate::request_probe::observe(config.session_id.as_deref(), &api_messages, tool_schemas);
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
    // Track whether the untagged-reasoning heuristic actually fired
    // (as opposed to reasoning_content being non-empty from <think> tags
    // or delta.extra.thinking). Without this flag, the is_untagged_reasoning_mode
    // check below would incorrectly route body text to reasoning after </think>,
    // because after </think>: reasoning_content is non-empty, content is empty,
    // and think_parser.is_in_think() is false — the same combination as
    // untagged-reasoning mode.
    let mut was_untagged_reasoning_detected = false;

    let activity_secs = config.stream_activity_interval_secs;
    // tight tripwire for the "SSE opened but no chunk ever
    // arrives" hang that pins the agent loop and the UI at
    // "正在思考…" until the 180 s outer watchdog fires. Once the
    // first chunk has arrived, this deadline is ignored (the
    // post-first-chunk stall path is still covered by
    // `stream_activity_stall_secs` notices and the outer timeout).
    let first_chunk_deadline =
        config.stream_first_chunk_timeout_secs.max(1);
    let collect_started = std::time::Instant::now();
    let mut first_chunk_arrived = false;
    // Post-first-chunk idle deadline (C1). The first-chunk tripwire only
    // covers "SSE opened, no data ever"; a stream that starts and then goes
    // silent used to degrade to a 24h sleep + a notice, leaving the turn to
    // the 180 s outer watchdog — which throws away the partial text the user
    // already saw. With this deadline a silent gap returns the partial
    // content as `Interrupted(finish_reason="stream_idle")`, so the visible
    // text is persisted and never replayed (no-duplicate-output rule).
    let idle_timeout_secs = config.stream_idle_timeout_secs;
    let idle_abort_dur = Duration::from_secs(idle_timeout_secs);
    let mut last_chunk_at = collect_started;

    if activity_secs == 0 {
        // `activity_secs == 0` is the "silence activity notices"
        // sentinel used by tests and bytelemetry-hungry operators.
        // even in that mode we still need to enforce the
        // first-chunk deadline — otherwise an SSE-open / no-chunk
        // hang pins the UI until the 180 s outer watchdog fires.
        const IDLE_ARM_MAX: Duration = Duration::from_secs(86_400);
        let first_chunk_deadline_dur = Duration::from_secs(first_chunk_deadline);
        let mut idle = Box::pin(sleep(first_chunk_deadline_dur));
        let mut stall_notice_sent = false;
        loop {
            tokio::select! {
                biased;
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        None => break,
                        Some(chunk_result) => {
                            if !first_chunk_arrived {
                                first_chunk_arrived = true;
                            }
                            // Any chunk re-arms the post-first-chunk idle
                            // deadline (C1): silence, not slowness, aborts.
                            last_chunk_at = std::time::Instant::now();
                            idle.as_mut().reset(
                                tokio::time::Instant::now()
                                    + if idle_abort_dur.is_zero() {
                                        IDLE_ARM_MAX
                                    } else {
                                        idle_abort_dur
                                    },
                            );
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
                                }, None));
                            }
                            let chunk = match chunk_result {
                                Ok(c) => c,
                                Err(e) => {
                                    // Stream error mid-stream — return
                                    // accumulated partial content as
                                    // Interrupted so it flows through
                                    // graceful_interrupt_result and
                                    // gets persisted to DB. Without
                                    // this, a network blip or API
                                    // error mid-stream drops all
                                    // accumulated content and the
                                    // session reloads empty.
                                    tracing::warn!(
                                        error = %e,
                                        "stream chunk error mid-stream — returning partial content for persistence"
                                    );
                                    let (flush_c, flush_r) = think_parser.flush();
                                    if !flush_c.is_empty() { content.push_str(&flush_c); }
                                    if !flush_r.is_empty() { reasoning_content.push_str(&flush_r); }
                                    tool_call_sanitizer.flush();
                                    let message = assemble_stream_assistant_message(
                                        &content, &reasoning_content, &tool_calls,
                                    );
                                    return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                        message,
                                        usage: last_usage.clone(),
                                        model: active_model.to_string(),
                                        finish_reason: Some("stream_error".to_string()),
                                    }, Some(e.to_string())));
                                }
                            };
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
                                &mut was_untagged_reasoning_detected,
                                on_chunk,
                            );
                        }
                    }
                }
                _ = idle.as_mut() => {
                    // After first chunk, silence is bounded by the idle
                    // deadline (C1): abort with the partial content instead
                    // of sleeping 24h until the outer watchdog discards it.
                    if first_chunk_arrived {
                        if !idle_abort_dur.is_zero()
                            && last_chunk_at.elapsed() >= idle_abort_dur
                        {
                            tracing::warn!(
                                elapsed_secs = last_chunk_at.elapsed().as_secs(),
                                idle_timeout_secs,
                                active_model = %active_model,
                                "stream idle after first chunk — aborting with partial content"
                            );
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                &format!(
                                    "Model output paused for {idle_timeout_secs}s — \
                                     stopping this reply and keeping what already arrived."
                                ),
                            );
                            let (flush_c, flush_r) = think_parser.flush();
                            if !flush_c.is_empty() { content.push_str(&flush_c); }
                            if !flush_r.is_empty() { reasoning_content.push_str(&flush_r); }
                            tool_call_sanitizer.flush();
                            let message = assemble_stream_assistant_message(
                                &content, &reasoning_content, &tool_calls,
                            );
                            return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                message,
                                usage: last_usage.clone(),
                                model: active_model.to_string(),
                                finish_reason: Some("stream_idle".to_string()),
                            }, Some(format!("stream idle for {idle_timeout_secs}s after first chunk"))));
                        }
                        if !stall_notice_sent {
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                "Still no model output — the stream may be stuck.",
                            );
                            stall_notice_sent = true;
                        }
                        idle.as_mut().reset(
                            tokio::time::Instant::now()
                                + if idle_abort_dur.is_zero() {
                                    IDLE_ARM_MAX
                                } else {
                                    idle_abort_dur.saturating_sub(last_chunk_at.elapsed())
                                        .max(Duration::from_millis(100))
                                },
                        );
                        continue;
                    }
                    // Pre-first-chunk: enforce the deadline.
                    let elapsed = collect_started.elapsed();
                    if elapsed >= first_chunk_deadline_dur {
                        tracing::warn!(
                            elapsed_secs = elapsed.as_secs(),
                            first_chunk_deadline_secs = first_chunk_deadline,
                            active_model = %active_model,
                            "no first chunk arrived within first_chunk_deadline — \
                             aborting stream early to recover from upstream hang"
                        );
                        emit_status(
                            config,
                            callbacks,
                            "activity",
                            &format!(
                                "No model output after {first_chunk_deadline}s — \
                                 the stream appears hung; retrying or check the \
                                 upstream provider."
                            ),
                        );
                        finish_reason = Some("no_first_chunk".to_string());
                        break;
                    }
                    idle.as_mut()
                        .reset(tokio::time::Instant::now() + first_chunk_deadline_dur);
                }
            }
        }
        // If we broke out due to no_first_chunk, `first_chunk_arrived`
        // is still false and `stream.next()` had never resolved.
        // `content` / `reasoning_content` / `tool_calls` are empty —
        // fall through to the assembly code below which will return
        // an empty assistant message with our stamped
        // `finish_reason`.
    } else {
        const IDLE_ARM_MAX: Duration = Duration::from_secs(86_400);
        let first_dur = Duration::from_secs(activity_secs);
        let stall_secs = config.stream_activity_stall_secs;
        let stall_dur = Duration::from_secs(stall_secs);
        let first_chunk_deadline_dur = Duration::from_secs(first_chunk_deadline);

        'silence_watch: loop {
            // when no chunk has arrived yet, the idle
            // arm is bounded by the time remaining until the
            // first-chunk deadline (capped at 1ms so it doesn't
            // degenerate to a busy-loop). Once the first chunk
            // arrives, this override is dropped and we fall back
            // to the legacy activity cadence (`first_dur`).
            // Note: `first_dur == 0` (the test "silence activity
            // notices" sentinel) used to short-circuit the
            // deadline tripwire because `min(0, remaining)` = 0.
            // We now treat `first_dur == 0` as "trust the deadline
            // first, fall back to 24h" — i.e. **off** the
            // activity notices but **on** the deadline.
            let initial_idle = if !first_chunk_arrived {
                let elapsed = collect_started.elapsed();
                let remaining = first_chunk_deadline_dur
                    .saturating_sub(elapsed)
                    .max(Duration::from_millis(1));
                if first_dur.is_zero() {
                    remaining
                } else {
                    first_dur.min(remaining)
                }
            } else if first_dur.is_zero() {
                Duration::from_secs(86_400)
            } else {
                first_dur
            };
            let mut idle = Box::pin(sleep(initial_idle));
            let mut warm_notice_sent = false;
            let mut stall_notice_sent = false;
            loop {
                tokio::select! {
                    biased;
                    chunk_opt = stream.next() => {
                        match chunk_opt {
                            None => break 'silence_watch,
                            Some(chunk_result) => {
                                if !first_chunk_arrived {
                                    first_chunk_arrived = true;
                                }
                                // Re-arm the post-first-chunk idle deadline
                                // (C1) — see the activity_secs==0 branch.
                                last_chunk_at = std::time::Instant::now();
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
                                }, None));
                                }
                                let chunk = match chunk_result {
                                    Ok(c) => c,
                                    Err(e) => {
                                        // Same rationale as the
                                        // activity_secs==0 path above:
                                        // return accumulated partial
                                        // content as Interrupted so it
                                        // gets persisted instead of
                                        // being dropped.
                                        tracing::warn!(
                                            error = %e,
                                            "stream chunk error mid-stream (silence_watch) — returning partial content for persistence"
                                        );
                                        let (flush_c, flush_r) = think_parser.flush();
                                        if !flush_c.is_empty() { content.push_str(&flush_c); }
                                        if !flush_r.is_empty() { reasoning_content.push_str(&flush_r); }
                                        tool_call_sanitizer.flush();
                                        let message = assemble_stream_assistant_message(
                                            &content, &reasoning_content, &tool_calls,
                                        );
                                        return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                            message,
                                            usage: last_usage.clone(),
                                            model: active_model.to_string(),
                                            finish_reason: Some("stream_error".to_string()),
                                        }, Some(e.to_string())));
                                    }
                                };
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
                                    &mut was_untagged_reasoning_detected,
                                    on_chunk,
                                );
                                continue 'silence_watch;
                            }
                        }
                    }
                    _ = idle.as_mut() => {
                        // the first-chunk deadline arm.
                        // We idle-limited by `remaining` above, so
                        // waking up here means we hit the deadline
                        // before any chunk arrived. Break out with a
                        // dedicated finish_reason so downstream can
                        // distinguish "provider never replied at all"
                        // from "outer watchdog timed out mid-stream".
                        if !first_chunk_arrived {
                            let elapsed = collect_started.elapsed();
                            if elapsed >= first_chunk_deadline_dur {
                                tracing::warn!(
                                    elapsed_secs = elapsed.as_secs(),
                                    first_chunk_deadline_secs = first_chunk_deadline,
                                    active_model = %active_model,
                                    "no first chunk arrived within first_chunk_deadline — \
                                     aborting stream early to recover from upstream hang"
                                );
                                emit_status(
                                    config,
                                    callbacks,
                                    "activity",
                                    &format!(
                                        "No model output after {first_chunk_deadline}s — \
                                         the stream appears hung; retrying or check the \
                                         upstream provider."
                                    ),
                                );
                                finish_reason = Some("no_first_chunk".to_string());
                                break 'silence_watch;
                            }
                            // First chunk has still not arrived but
                            // we haven't reached the deadline yet —
                            // emit warm/stall notices and re-arm idle
                            // so we keep tightening toward the
                            // deadline while informing the UI.
                            let elapsed = collect_started.elapsed();
                            let remaining = first_chunk_deadline_dur
                                .saturating_sub(elapsed)
                                .max(Duration::from_millis(1));
                            if !warm_notice_sent {
                                emit_status(
                                    config,
                                    callbacks,
                                    "activity",
                                    "Still waiting for the model…",
                                );
                                warm_notice_sent = true;
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + stall_dur.min(remaining));
                            } else if stall_secs > 0 && !stall_notice_sent {
                                emit_status(
                                    config,
                                    callbacks,
                                    "activity",
                                    "Still no model output — the stream may be stuck.",
                                );
                                stall_notice_sent = true;
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + remaining);
                            } else {
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + remaining);
                            }
                            continue;
                        }
                        // Idle abort (C1): silence longer than the deadline
                        // ends the reply with the partial content, instead of
                        // holding the turn until the 180 s outer watchdog
                        // discards everything already shown.
                        if !idle_abort_dur.is_zero()
                            && last_chunk_at.elapsed() >= idle_abort_dur
                        {
                            tracing::warn!(
                                elapsed_secs = last_chunk_at.elapsed().as_secs(),
                                idle_timeout_secs,
                                active_model = %active_model,
                                "stream idle after first chunk (silence_watch) — aborting with partial content"
                            );
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                &format!(
                                    "Model output paused for {idle_timeout_secs}s — \
                                     stopping this reply and keeping what already arrived."
                                ),
                            );
                            let (flush_c, flush_r) = think_parser.flush();
                            if !flush_c.is_empty() { content.push_str(&flush_c); }
                            if !flush_r.is_empty() { reasoning_content.push_str(&flush_r); }
                            tool_call_sanitizer.flush();
                            let message = assemble_stream_assistant_message(
                                &content, &reasoning_content, &tool_calls,
                            );
                            return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                message,
                                usage: last_usage.clone(),
                                model: active_model.to_string(),
                                finish_reason: Some("stream_idle".to_string()),
                            }, Some(format!("stream idle for {idle_timeout_secs}s after first chunk"))));
                        }
                        // Re-arm toward the idle abort deadline, never to 24h:
                        // a 24h re-arm would push the `stream_idle` check out
                        // past the 180 s outer watchdog, making the whole
                        // post-first-chunk abort dead code (the exact bug this
                        // deadline was added to fix).
                        let next_arm = if idle_abort_dur.is_zero() {
                            IDLE_ARM_MAX
                        } else {
                            idle_abort_dur
                                .saturating_sub(last_chunk_at.elapsed())
                                .max(Duration::from_millis(100))
                        };
                        if !warm_notice_sent {
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                "Still waiting for the model…",
                            );
                            warm_notice_sent = true;
                            let arm = if stall_secs > 0 {
                                stall_dur.min(next_arm)
                            } else {
                                next_arm
                            };
                            idle.as_mut().reset(tokio::time::Instant::now() + arm);
                        } else if stall_secs > 0 && !stall_notice_sent {
                            emit_status(
                                config,
                                callbacks,
                                "activity",
                                "Still no model output — the stream may be stuck.",
                            );
                            stall_notice_sent = true;
                            idle.as_mut().reset(tokio::time::Instant::now() + next_arm);
                        } else {
                            idle.as_mut().reset(tokio::time::Instant::now() + next_arm);
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

    // last-line defence for models that ignore the system
    // prompt and emit an unwrapped reasoning monologue as visible content.
    // If the whole reply reads like internal analysis ("I think...",
    // "trying to reconstruct...", etc.) and never used a ` <think` tag,
    // route it to `reasoning_content` so the UI renders it as a collapsed
    // thinking block instead of flooding the chat.
    // Skip this when tool_calls are present — the text before a tool call
    // is the model's visible output, not reasoning.
    if !content.is_empty()
        && reasoning_content.is_empty()
        && tool_calls.is_empty()
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
        } else {
            // Second chance: wrapper markup corrupted but the inner
            // `<invoke>` block intact (real-device debris
            // `]<]minimax[>[`). Normalize the wrapper markers and retry.
            let tolerant = hermix_core::tool_call_parser::parse_tool_calls_tolerant(&raw_content);
            if !tolerant.is_empty() {
                tracing::warn!(
                    recovered_count = tolerant.len(),
                    raw_len = raw_content.len(),
                    "Recovered tool call(s) from corrupted wrapper markup via tolerant parse"
                );
                tool_calls = tolerant;
            } else if let Some(excerpt) = lost_tool_call_fragment_excerpt(&raw_content) {
                // 静默失败 → 显式告警。此前该路径完全无声：模型试图以文本
                // XML 发工具调用，标记破损到解析器无法识别（真机残骸
                // `]<]minimax[>[`）→ sanitizer 剥掉可识别部分、解析 0 个、
                // 残留文本再被 untagged-reasoning 启发式挪进 reasoning →
                // finalizer 直接 finalize，用户只见"跑着就停了"，日志无定位
                // 线索。恢复手段应在解析层（事件级流式解析状态机，插件内核
                // 批次），此处只负责让失败可见。
                tracing::warn!(
                    raw_len = raw_content.len(),
                    excerpt = %excerpt,
                    "tool-call XML fragments present but unparseable; tool call LOST — turn will finalize without dispatch"
                );
            }
        }
    }

    // Recover degenerate tool-call names (MiniMax-M3 streaming compat).
    // The non-streaming Anthropic adapter (anthropic_adapter.rs) already
    // extracts the real name from the nested `input` object; mirror it here
    // so streaming turns don't silently finalize into a fragment when the
    // `name` delta never arrives. The real name is delivered inside the
    // tool call's `arguments` JSON (`name` / `tool` / `function.name`).
    for tc in &mut tool_calls {
        if tc.function.name.is_empty() || tc.function.name == "tool_call" {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
            {
                for key in ["name", "tool", "function"] {
                    if let Some(v) = map.get(key) {
                        let extracted = if key == "function" {
                            v.get("name").and_then(|n| n.as_str())
                        } else {
                            v.as_str()
                        };
                        if let Some(n) = extracted {
                            if !n.is_empty() && n != "tool_call" {
                                tc.function.name = n.to_string();
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // Filter out tool calls with genuinely unrecoverable empty function
    // names. Was added to avoid the autonomous nudge loop when a placeholder
    // entry's name is truly missing; now only fires after the recovery pass
    // above has had a chance to fix degenerate names.
    let empty_name_count = tool_calls.iter().filter(|tc| tc.function.name.is_empty()).count();
    if empty_name_count > 0 {
        tracing::warn!(
            total = tool_calls.len(),
            empty_names = empty_name_count,
            "Tool calls with unrecoverable empty function names dropped; turn will finalize"
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

/// 检测 raw_content 中工具调用标记的破损残留——解析器无法识别、sanitizer
/// 剥除后仍剩下的碎片（真机故障残骸 `]<]minimax[>`：`<minimax:tool_call>`
/// 被模型自身输出损坏，`:tool_call` 后缀缺失、括号错乱）。返回单行摘录
/// （命中点前后各 80 字符、压平换行）供告警；`None` = 无碎片（正常纯文本）。
///
/// 仅在 `parse_tool_calls` 解析为 0 时调用——完整可解析的调用不进这里；
/// 大写 "MiniMax"（散文提及模型名的常见写法）不命中任何小写标记。
fn lost_tool_call_fragment_excerpt(raw: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "<minimax:tool_call",
        "</minimax:tool_call",
        "<invoke",
        "</invoke",
        "<function_calls",
        "</function_calls",
        // 破损变体：`]` 紧贴小写标记名（正常代码/散文几乎不可能出现
        // `]minimax` 序列——数组下标是 `[minimax]`，泛型是 `>minimax`）。
        "]minimax",
    ];
    let pos = MARKERS.iter().find_map(|m| raw.find(m))?;
    let start = clip_to_char_boundary(raw, pos.saturating_sub(80));
    let end = clip_to_char_boundary(raw, raw.len().min(pos + 80));
    Some(raw[start..end].replace(['\n', '\r'], " "))
}

/// 向后调整到最近的 char boundary（`floor_char_boundary` 尚未稳定）。
fn clip_to_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupt::InterruptController;
    use crate::oauth_refresh::OAuthRefresh;
    use async_stream::stream;
    use futures::stream::BoxStream;
    use hermix_core::{AgentError, FunctionCallDelta, LlmProvider, LlmResponse, StreamDelta, ToolCallDelta};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 真机故障残骸（`]<]minimax[>[`）必须被检出并给出摘录；
    /// 正常纯文本与散文提及 MiniMax（大写）不触发。
    #[test]
    fn lost_tool_call_fragment_excerpt_detects_damaged_marker() {
        let raw = "让我开始第一批：\n]<]minimax[>\n]<]minimax[>";
        let e = lost_tool_call_fragment_excerpt(raw).expect("破损标记应被检出");
        assert!(e.contains("]minimax"), "摘录应含命中片段: {e}");
        assert!(!e.contains('\n'), "摘录应压平换行: {e}");
        assert!(lost_tool_call_fragment_excerpt("普通回复，没有标记。").is_none());
        assert!(lost_tool_call_fragment_excerpt("我是 MiniMax-M3 模型。").is_none());
    }

    /// when a single
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
        let mut was_untagged_reasoning_detected = false;
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
            &mut was_untagged_reasoning_detected,
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
        let mut was_untagged_reasoning_detected = false;
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
            &mut was_untagged_reasoning_detected,
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
    // hard outer timeout tests for `collect_stream_llm_response`.
    //Symptom : provider opens the SSE connection and then
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
    /// The first-chunk deadline is set well above the overall
    /// timeout so the existing `hard_timeout_*` tests still exercise
    /// the outer watchdog path (the tighter "no_first_chunk" arm
    /// is covered by `first_chunk_timeout_*` tests below).
    async fn drive_collect(
        provider: Arc<dyn LlmProvider>,
        stream_overall_timeout_secs: u64,
    ) -> Result<StreamCollectOutcome, AgentError> {
        // Give the first-chunk deadline 1 s of slack over the
        // overall timeout so the outer watchdog wins first.
        let first_chunk =
            stream_overall_timeout_secs.saturating_add(1).max(120);
        drive_collect_with(provider, stream_overall_timeout_secs, first_chunk).await
    }

    /// same as [`drive_collect`] but lets the caller
    /// also override `stream_first_chunk_timeout_secs` for tests
    /// that need to observe the new first-chunk tripwire directly.
    async fn drive_collect_with(
        provider: Arc<dyn LlmProvider>,
        stream_overall_timeout_secs: u64,
        stream_first_chunk_timeout_secs: u64,
    ) -> Result<StreamCollectOutcome, AgentError> {
        let mut config = AgentConfig::default();
        // Silence the inner activity watchdog so it doesn't bias
        // timing measurements. The hard timeout is what we're
        // testing here.
        config.stream_activity_interval_secs = 0;
        config.stream_overall_timeout_secs = stream_overall_timeout_secs;
        config.stream_first_chunk_timeout_secs = stream_first_chunk_timeout_secs;

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

    /// C1: a stream that emits ONE chunk then goes silent must end with
    /// `finish_reason = "stream_idle"` and keep the partial text. Before
    /// this deadline existed the turn hung on a 24h idle arm until the
    /// outer watchdog returned an EMPTY reply — discarding text the user
    /// had already seen.
    #[tokio::test]
    async fn idle_after_first_chunk_aborts_with_partial_content() {
        struct OneThenSilent;

        #[async_trait::async_trait]
        impl LlmProvider for OneThenSilent {
            async fn chat_completion(
                &self,
                _messages: &[hermix_core::Message],
                _tools: &[hermix_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                unreachable!("stream path only")
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
                            content: Some("部分内容".to_string()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                    // Then silence forever — the provider stops pushing
                    // while the TCP stream stays open.
                    std::future::pending::<()>().await;
                })
            }
        }

        let mut config = AgentConfig::default();
        config.stream_activity_interval_secs = 0;
        config.stream_first_chunk_timeout_secs = 5;
        config.stream_idle_timeout_secs = 1;
        config.stream_overall_timeout_secs = 30;

        let oauth = OAuthRefresh::new();
        let interrupt = InterruptController::new();
        let ctx = crate::context::ContextManager::new(2048);
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> = Box::new(|_: StreamChunk| {});
        let provider: Arc<dyn LlmProvider> = Arc::new(OneThenSilent);

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            crate::stream_collect::collect_stream_llm_response(
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
            ),
        )
        .await
        .expect("idle watchdog never fired — stream hung")
        .expect("collect must not error");

        match outcome {
            StreamCollectOutcome::Interrupted(resp, err) => {
                assert_eq!(
                    resp.finish_reason.as_deref(),
                    Some("stream_idle"),
                    "expected stream_idle finish_reason"
                );
                let content = resp.message.content.clone().unwrap_or_default();
                assert!(
                    content.contains("部分内容"),
                    "partial content must be preserved, got: {content:?}"
                );
                assert!(err.is_some(), "idle abort must report an error string");
            }
            other => panic!("expected Interrupted(stream_idle), got {other:?}"),
        }
    }

    /// Provider hangs forever → outer watchdog fires after
    /// `stream_overall_timeout_secs` (1 s) and the wrapper returns
    /// `StreamCollectOutcome::Complete { finish_reason: "timeout" }`
    /// with an empty assistant message. Without the watchdog this
    /// test would never return — the agent loop would stay pinned
    /// at "正在思考" forever (the bug).
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

    /// round 29 follow-up. When the provider opens the
    /// streaming connection but never sends the **first** chunk,
    /// the new `stream_first_chunk_timeout_secs` tripwire aborts
    /// the inner collect early with `finish_reason="no_first_chunk"`.
    /// This turns the "approval+hang forever" symptom
    /// into a 1 s recovery for test instrumentation, well inside
    /// the 180 s outer watchdog, so the agent loop reaches
    /// `decide_empty_response` quickly enough to nudge / finalise
    /// instead of pinning the UI at "正在思考".
    #[tokio::test]
    async fn first_chunk_timeout_fires_when_no_chunk_arrives() {
        // Outer timeout generous (10s) — if the first-chunk tripwire
        // were missing, the test would either hang past the
        // wall-clock cap below or fire on the outer timeout instead.
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            drive_collect_with(
                Arc::new(HangingProvider),
                /* stream_overall_timeout_secs = */ 10,
                /* stream_first_chunk_timeout_secs = */ 1,
            ),
        )
        .await
        .expect("collect_stream_llm_response hung past 10s — neither watchdog fired")
        .expect("collect_stream_llm_response returned Err");

        match outcome {
            StreamCollectOutcome::Complete(resp) => {
                assert_eq!(
                    resp.finish_reason.as_deref(),
                    Some("no_first_chunk"),
                    "expected finish_reason=\"no_first_chunk\", got {:?}",
                    resp.finish_reason
                );
                assert!(
                    resp.message.content.as_deref().unwrap_or("").is_empty(),
                    "expected empty content on first-chunk timeout, got {:?}",
                    resp.message.content
                );
            }
            other => panic!(
                "expected StreamCollectOutcome::Complete, got {:?}",
                other
            ),
        }
    }

    /// once the first chunk has arrived, the
    /// first-chunk deadline is no longer applicable; the
    /// stream continues normally and the provider's `stop`
    /// finish_reason is honoured.
    #[tokio::test]
    async fn first_chunk_timeout_is_noop_after_first_chunk() {
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            drive_collect_with(
                Arc::new(QuickProvider),
                /* stream_overall_timeout_secs = */ 10,
                /* stream_first_chunk_timeout_secs = */ 1,
            ),
        )
        .await
        .expect("collect_stream_llm_response hung past 5s — false-positive first-chunk timeout?")
        .expect("collect_stream_llm_response returned Err");

        match outcome {
            StreamCollectOutcome::Complete(resp) => {
                // QuickProvider sends a chunk almost immediately,
                // so the first-chunk deadline never trips — the
                // provider's own `stop` finish_reason is preserved.
                assert_eq!(
                    resp.finish_reason.as_deref(),
                    Some("stop"),
                    "expected finish_reason=\"stop\", got {:?}",
                    resp.finish_reason
                );
                assert_eq!(
                    resp.message.content.as_deref(),
                    Some("hi"),
                    "expected content=\"hi\", got {:?}",
                    resp.message.content
                );
            }
            other => panic!(
                "expected StreamCollectOutcome::Complete, got {:?}",
                other
            ),
        }
    }

    // ================================================================
    //Fault-injection tests
    // ================================================================
    // Each test injects a specific failure mode and verifies the
    // agent loop's stream collection handles it gracefully (no hang,
    // no panic, returns a structured error or timeout).

    /// Mock provider that returns a 503 error on every call.
    /// Models LLM service unavailable.
    struct Error503Provider;

    #[async_trait::async_trait]
    impl LlmProvider for Error503Provider {
        async fn chat_completion(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Err(AgentError::LlmApi("503 Service Unavailable".to_string()))
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
                yield Err(AgentError::LlmApi("503 Service Unavailable".to_string()));
            })
        }
    }

    /// Fault injection: LLM returns 503 error → stream collection
    /// must surface the error as an `Interrupted` outcome (not hang,
    /// not return `Err` — the collector wraps stream errors into a
    /// graceful `Interrupted` response with `finish_reason = "stream_error"`).
    #[tokio::test]
    async fn fault_injection_llm_503_returns_error() {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_collect_with(
                Arc::new(Error503Provider),
                10,
                10,
            ),
        )
        .await
        .expect("collect_stream_llm_response hung past 5s on 503");

        // The collector catches stream errors and returns them as
        // Ok(Interrupted(...)) with finish_reason = "stream_error".
        // This is the designed behavior — the caller (agent loop) sees
        // a partial response and can decide how to handle it.
        assert!(
            result.is_ok(),
            "expected Ok for 503 (Interrupted), got Err: {:?}",
            result.err()
        );
        let outcome = result.unwrap();
        match outcome {
            StreamCollectOutcome::Interrupted(resp, _) => {
                assert!(
                    resp.finish_reason.as_deref() == Some("stream_error")
                        || resp.finish_reason.as_deref() == Some("timeout"),
                    "expected stream_error or timeout finish_reason for 503, got: {:?}",
                    resp.finish_reason
                );
            }
            StreamCollectOutcome::Complete(resp) => {
                // Some code paths may return Complete with an error
                // finish_reason — that's also acceptable as long as
                // we didn't hang.
                assert!(
                    resp.finish_reason.as_deref().unwrap_or("").contains("error")
                        || resp.finish_reason.as_deref().unwrap_or("").contains("timeout"),
                    "expected error/timeout finish_reason for 503, got: {:?}",
                    resp.finish_reason
                );
            }
        }
    }

    /// Mock provider whose stream panics mid-way. Models a tool
    /// handler or LLM provider internal panic.
    struct PanicProvider;

    #[async_trait::async_trait]
    impl LlmProvider for PanicProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            panic!("injected panic: tool handler exploded")
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
                yield Err(AgentError::ToolExecution(
                    "injected panic: tool handler exploded".to_string()
                ));
            })
        }
    }

    /// Fault injection: provider panics → stream collection must
    /// not hang (panic propagates as stream error or task death).
    /// We wrap in timeout to detect hangs.
    #[tokio::test]
    async fn fault_injection_provider_panic_does_not_hang() {
        // The panic will propagate through the stream. We use
        // timeout to ensure it doesn't hang.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_collect_with(
                Arc::new(PanicProvider),
                10,
                10,
            ),
        ).await;

        // Either the panic propagates as an error, or the task
        // is cancelled by the runtime. Either way, we must NOT hang.
        assert!(
            result.is_ok(),
            "collect_stream_llm_response hung past 5s on provider panic"
        );
    }

    /// Fault injection: WS disconnect simulation. The stream
    /// collection should handle a dropped consumer gracefully
    /// (the on_chunk callback is simply never called again).
    /// The stream_overall_timeout fires and the outcome is either
    /// `Interrupted` (first-chunk timeout) or `Complete` (overall
    /// timeout) — both are acceptable as long as we don't hang.
    #[tokio::test]
    async fn fault_injection_ws_disconnect_produces_timeout() {
        // HangingProvider models a WS that went silent (no data,
        // no close frame). The stream_overall_timeout should fire.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            drive_collect_with(
                Arc::new(HangingProvider),
                /* stream_overall = */ 1, // 1s overall timeout
                /* first_chunk = */ 1,
            ),
        )
        .await
        .expect("collect_stream_llm_response hung past 5s — overall timeout failed");

        // The collector wraps timeouts into a graceful outcome
        // (either Interrupted or Complete with finish_reason = "timeout").
        // The key invariant: we did NOT hang.
        assert!(
            result.is_ok(),
            "expected Ok for WS disconnect, got Err: {:?}",
            result.err()
        );
        let outcome = result.unwrap();
        match outcome {
            StreamCollectOutcome::Interrupted(_, _) => {
                // First-chunk timeout fired — provider went silent.
                // This is the expected path when both timeouts are 1s.
            }
            StreamCollectOutcome::Complete(resp) => {
                assert!(
                    resp.finish_reason.as_deref() == Some("timeout")
                        || resp.finish_reason.as_deref() == Some("no_first_chunk"),
                    "expected timeout or no_first_chunk finish_reason on WS disconnect, got {:?}",
                    resp.finish_reason
                );
            }
        }
    }

    /// MiniMax-M3 streaming compat: the `name` delta never arrives while the
    /// real name rides inside the `arguments` JSON. The flush-time recovery
    /// pass must backfill `function.name` from `arguments` instead of
    /// silently dropping the call (which would nudge the autonomous loop).
    ///
    /// Covers both degenerate shapes seen in the wild:
    /// - `name` absent entirely,
    /// - `name` delivered as the placeholder `"tool_call"`.
    struct NamelessToolCallProvider {
        /// `None` → name delta absent; `Some(s)` → degenerate name delta.
        name_delta: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for NamelessToolCallProvider {
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
                message: hermix_core::Message::assistant(String::new()),
                usage: None,
                model: String::from("minimax-m3"),
                finish_reason: Some(String::from("tool_calls")),
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
            let name_delta = self.name_delta;
            Box::pin(stream! {
                yield Ok(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index: 0,
                            id: Some(String::from("call_m3_1")),
                            function: Some(FunctionCallDelta {
                                name: name_delta.map(String::from),
                                arguments: Some(String::from(
                                    "{\"name\":\"read_file\",\"path\":\"a.txt\"}",
                                )),
                            }),
                        }]),
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
                yield Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some(String::from("tool_calls")),
                    usage: None,
                });
            })
        }
    }

    #[tokio::test]
    async fn minimax_m3_missing_name_delta_recovers_from_arguments() {
        for name_delta in [None, Some("tool_call")] {
            let provider = Arc::new(NamelessToolCallProvider { name_delta });
            let outcome = drive_collect(provider, /* stream_overall = */ 5)
                .await
                .expect("stream should complete");
            let outcome = match outcome {
                StreamCollectOutcome::Complete(resp) => resp,
                other => panic!("expected Complete, got {:?}", other),
            };
            let msg = &outcome.message;
            let tcs = msg
                .tool_calls
                .as_ref()
                .expect("tool_calls must survive the nameless-name flush");
            assert_eq!(tcs.len(), 1, "degenerate tool call must not be dropped");
            assert_eq!(
                tcs[0].function.name, "read_file",
                "name must be recovered from the arguments JSON (name_delta={:?})",
                name_delta
            );
            assert_eq!(
                tcs[0].function.arguments, "{\"name\":\"read_file\",\"path\":\"a.txt\"}",
                "arguments must be preserved verbatim"
            );
        }
    }
}
