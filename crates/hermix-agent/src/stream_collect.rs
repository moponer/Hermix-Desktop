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

use hermix_core::tool_call_sanitizer::ToolCallSanitizer;
use hermix_core::{Message, StreamChunk, ToolCall, UsageStats};

use crate::agent_helpers::AgentCallbacks;
use crate::python_alignment::ThinkBlockParser;

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
            }
        }
        if let Some(ref extra) = delta.extra {
            if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                reasoning_content.push_str(thinking);
                if let Some(ref cb) = callbacks.on_thinking {
                    cb(thinking);
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