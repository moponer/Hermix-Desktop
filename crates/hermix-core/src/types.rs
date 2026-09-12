use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MessageRole
// ---------------------------------------------------------------------------

/// Role of a message participant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    #[default]
    User,
    Assistant,
    Tool,
}

// ---------------------------------------------------------------------------
// CacheControl
// ---------------------------------------------------------------------------

/// Cache hint type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    Ephemeral,
    Persistent,
}

/// Cache control annotation for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    pub cache_type: CacheType,
}

// ---------------------------------------------------------------------------
// FunctionCall / ToolCall
// ---------------------------------------------------------------------------

/// A function call within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A tool call emitted by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(flatten)]
    pub function: FunctionCall,
}

// ---------------------------------------------------------------------------
// ReasoningContent
// ---------------------------------------------------------------------------

/// Format of the reasoning content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningFormat {
    Simple,
    Details,
}

/// Reasoning content parsed from the LLM response.
/// Supports multiple formats used by different providers:
/// - `reasoning_content` (simple string)
/// - `reasoning` (simple string)
/// - `reasoning_details` (structured array)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningContent {
    pub text: String,
    pub format: ReasoningFormat,
}

impl ReasoningContent {
    /// Parse reasoning content from a raw JSON value.
    /// Accepts:
    /// - A plain string (from `reasoning_content` or `reasoning`)
    /// - An object with a `text` field
    /// - An array of objects with `text` fields (from `reasoning_details`)
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(s) => Some(ReasoningContent {
                text: s.clone(),
                format: ReasoningFormat::Simple,
            }),
            serde_json::Value::Object(map) => {
                map.get("text")
                    .and_then(|v| v.as_str())
                    .map(|text| ReasoningContent {
                        text: text.to_string(),
                        format: ReasoningFormat::Details,
                    })
            }
            serde_json::Value::Array(arr) => {
                let text = arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    None
                } else {
                    Some(ReasoningContent {
                        text,
                        format: ReasoningFormat::Details,
                    })
                }
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ContentPart (for vision / multimodal messages)
// ---------------------------------------------------------------------------

/// A single content part in a multimodal message.
/// Serializes to the OpenAI / Claude / MiniMax vision format:
/// `{"type":"text","text":"..."}` or
/// `{"type":"image_url","image_url":{"url":"data:image/..."}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single message in a conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Multimodal content (text + image for vision models).
    /// When this is `Some(...)`, the provider serializes `content`
    /// as a JSON array; `content` (string) is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_parts: Option<Vec<ContentPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    /// Execution timeline events emitted during this assistant turn
    /// (tool_start / tool_stdout / tool_complete / status /
    /// file_change). Persisted by the agent loop so that a session
    /// reload rebuilds the same inline tool cards the user saw
    /// mid-stream, including correct "已完成" / "失败" status,
    /// arguments, and stdout output.
    /// added — previously the timeline was only
    /// streamed over WS, so on reload the frontend had no way to
    /// know whether a tool call was running, succeeded or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_timeline: Option<Vec<ExecutionTimelineEvent>>,
    /// Frontend-built chronological reasoning/tools/text timeline for
    /// this assistant turn (`unified_steps`). The desktop client assembles
    /// it live from the WS stream (reasoning chunk → text chunk → tool
    /// event → …) so the inline cards show tools *where they happened*
    /// in the conversation flow, not grouped at the end. We persist it
    /// verbatim (JSON array of `{kind, ...}` step objects) so a session
    /// reload restores the exact same interleaving via
    /// `resolveSteps(message.unified_steps)` instead of falling back to
    /// the fixed reasoning→tools→text `buildFallbackSteps` order.
    /// `None` for non-assistant roles and for messages saved before this
    /// column existed (older clients simply rebuild via the fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_steps: Option<Vec<serde_json::Value>>,
    /// ephemeral messages exist only in the
    /// in-memory context — they are **never** persisted to the DB.
    /// Used for replay-path `tool_result` messages that satisfy the
    /// provider API's `tool_call_id` pairing requirement without
    /// creating duplicate DB rows (the original execution's
    /// `tool_result` is already persisted). Skipping persistence at
    /// the source eliminates the need for downstream dedup logic
    /// in the frontend.
    #[serde(default, skip)]
    pub ephemeral: bool,
}

/// One entry in an assistant message's execution timeline.
/// Mirrors the frontend `ExecutionTimelineEvent` interface 1:1 so
/// the chat layer can feed them straight into
/// `expandTimelineToMessages` / `groupTimeline` without
/// per-field translation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionTimelineEvent {
    /// Event kind.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Tool name (for `tool_start` / `tool_stdout` / `tool_complete`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Event payload — tool_start args, tool_stdout chunk,
    /// tool_complete result, status text, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// JSON-encoded tool arguments (for `tool_start`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    /// Stdout chunk index (for `tool_stdout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_index: Option<u64>,
    /// Stdout chunk total (for `tool_stdout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_total: Option<u64>,
    /// Server-side event timestamp (RFC3339).
    pub created_at: String,
    /// Stable WS `tool_call_id`; used by the timeline parser to
    /// group stdout chunks with their parent invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// file_change-only fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_summary: Option<String>,
    /// Single source of truth for whether a `tool_complete` event
    /// represents a failure (`ToolResult::is_error`). Persisted so a
    /// reloaded session marks failed tool cards from this flag instead
    /// of re-guessing from the prose payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl Message {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: Some(content.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: Some(content.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create an assistant message with text content.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: Some(content.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create an assistant message with tool calls.
    pub fn assistant_with_tool_calls(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content,
            content_parts: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        name: Option<impl Into<String>>,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Some(content.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            // Persist the originating tool name alongside the
            // tool result so reloaded sessions can rebuild
            // `ToolMessageMeta` with the real label (terminal /
            // read_file / list_files / etc.) instead of falling
            // back to a generic "tool". The argument
            // `name: Some("terminal")` is a cheap one-line
            // `Option<String>` rather than a `String` so legacy
            // call sites that don't know the name can pass
            // `None`.
            name: name.map(Into::into),
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create a user message with text + inline image (vision support).
    /// `image_base64` is the raw base64 string (without the data URI prefix);
    /// we wrap it as `data:<mime>;base64,<...>` for the LLM API.
    pub fn user_with_image(text: &str, image_base64: &str, mime_type: &str) -> Self {
        let url = format!("data:{mime_type};base64,{image_base64}");
        Self {
            role: MessageRole::User,
            content: None,
            content_parts: Some(vec![
                ContentPart::Text { text: text.to_string() },
                ContentPart::ImageUrl { image_url: ImageUrl { url } },
            ]),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }

    /// Create a user message with text + multiple inline images .
    /// Each tuple is `(mime_type, base64_data)`. The LLM receives a
    /// multimodal content array: one Text part followed by N ImageUrl parts.
    pub fn user_with_images(text: &str, images: &[(&str, &str)]) -> Self {
        let mut parts = vec![ContentPart::Text { text: text.to_string() }];
        for (mime_type, base64) in images {
            let url = format!("data:{mime_type};base64,{base64}");
            parts.push(ContentPart::ImageUrl { image_url: ImageUrl { url } });
        }
        Self {
            role: MessageRole::User,
            content: None,
            content_parts: Some(parts),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolResult
// ---------------------------------------------------------------------------

/// Result from executing a tool call.
/// `#[must_use]` because silently discarding a tool result loses the
/// entire response — the dispatch loop is required to surface it to
/// the LLM via `tool` role messages or the conversation breaks.
#[must_use = "ToolResult is the LLM-facing response and must be returned to the agent loop, not silently dropped"]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// (A+C product fix): when a tool call was *denied*
    /// rather than failed-on-execution, this carries the structured
    /// `DenialKind` **snake_case wire value** (`user_denied` / `timed_out`
    /// `channel_closed` / `policy`). `None` for normal successes and
    /// genuine execution errors. (The `DenialKind` enum variants are
    /// PascalCase, but `as_str()` serializes them to snake_case for the
    /// wire.) `skip_serializing_if` keeps the field invisible to the
    /// persisted `tool_result` rows and the provider API (both ignore
    /// it), so this is purely an in-process signal that the
    /// `tool_result_processor` uses to inject the "tell the user what
    /// happened" system note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denial_kind: Option<String>,
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn ok(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
            denial_kind: None,
        }
    }

    /// Create an error tool result.
    pub fn err(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
            denial_kind: None,
        }
    }

    /// Create a *denial* tool result — the call never executed because
    /// the user refused, the timeout fired, or the channel dropped.
    /// `kind` names the structured reason so the result processor can
    /// phrase the user-facing note correctly.
    pub fn denied(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
            denial_kind: Some(kind.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// UsageStats / ToolErrorRecord / AgentResult
// ---------------------------------------------------------------------------

/// Token usage statistics from an LLM response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    /// Tokens read from the provider's prompt cache
    /// (e.g. Anthropic `cache_read_input_tokens`). 0 when the
    /// provider does not support prompt caching or the response
    /// did not include cache data.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_read_tokens: u64,
    /// Tokens written to the provider's prompt cache
    /// (e.g. Anthropic `cache_creation_input_tokens`). 0 when
    /// the provider does not support prompt caching.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_creation_tokens: u64,
    /// Tokens billed for hidden reasoning / chain-of-thought
    /// (e.g. OpenAI o1-series `completion_tokens_details.reasoning_tokens`,
    /// Anthropic extended-thinking). Distinct from `completion_tokens`
    /// because reasoning is invisible to the user but counts toward
    /// the bill. 0 when the provider does not break this out.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub reasoning_tokens: u64,
}

impl UsageStats {
    /// Build `UsageStats` from a provider's `usage` JSON object.
    ///
    /// Handles multiple known API response formats with a single
    /// unified extraction path — built-in and custom providers both
    /// benefit without per-provider parsing:
    ///
    /// | Format | prompt/completion | cache_read | cache_creation | reasoning |
    /// |--------|-------------------|------------|----------------|-----------|
    /// | OpenAI Chat Completions | `prompt_tokens` / `completion_tokens` | `prompt_tokens_details.cached_tokens` | — | `completion_tokens_details.reasoning_tokens` |
    /// | Anthropic Messages | `input_tokens` / `output_tokens` | `cache_read_input_tokens` | `cache_creation_input_tokens` | `reasoning_tokens` |
    /// | OpenAI Responses | `input_tokens` / `output_tokens` | `cache_read_input_tokens` | `cache_creation_input_tokens` | — |
    ///
    /// Fields absent from the response default to 0. `estimated_cost` is
    /// always `None` — cost calculation is external (`CostGuard`).
    pub fn from_json(usage: &serde_json::Value) -> Self {
        // Try both naming conventions
        let prompt = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| usage.get("input_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let completion = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| usage.get("output_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let total = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(prompt + completion);

        // Cache read: Anthropic / OpenAI Responses top-level, or
        // OpenAI Chat Completions nested in prompt_tokens_details
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);

        // Cache creation: Anthropic / OpenAI Responses only
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Reasoning: Anthropic top-level, or OpenAI nested
        let reasoning = usage
            .get("reasoning_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("completion_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);

        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            estimated_cost: None,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            reasoning_tokens: reasoning,
        }
    }
}

/// Helper for `skip_serializing_if` on `u64` zero values.
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Record of a tool error during execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolErrorRecord {
    pub tool_name: String,
    pub error: String,
    pub turn: u32,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Final result of an agent run.
/// `#[must_use]` because dropping the return value of a run means
/// losing the entire conversation arc, usage stats, compression
/// handoff, and any tool error records — callers always want to
/// persist or surface at least some of these fields.
#[must_use = "AgentResult is the agent loop's final output; dropping it loses usage stats, compression handoff, and tool errors"]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgentResult {
    pub messages: Vec<Message>,
    pub finished_naturally: bool,
    pub total_turns: u32,
    #[serde(default)]
    pub tool_errors: Vec<ToolErrorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
    /// Set when the loop stopped due to [`crate::AgentError::Interrupted`] (Python parity).
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    /// Estimated session spend in USD when cost tracking is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cost_usd: Option<f64>,
    /// Hook / plugin parity: `on_session_start` ran this run (new session, not restored prompt).
    #[serde(default, skip_serializing_if = "is_false")]
    pub session_started_hooks_fired: bool,
    /// Most recent streaming finish_reason from the last model call.
    /// `"stop"` / `"length"` / `"tool_calls"` / `"interrupted"` / `None`.
    /// Lets the WS handler emit a single `StreamEvent::Stop` so the UI
    /// can show a "已截断 / 已中断 / 已完成" chip without re-parsing chunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Raw backend error text captured when the loop failed (e.g. an
    /// LLM 400 "invalid function arguments", a 5xx, or a transport
    /// error). Surfaced to the frontend as the muted/collapsible
    /// "技术详情" subtext of the error card so the user sees the real
    /// cause instead of a generic "backend error" message. `None` for
    /// clean completions / user interrupts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Compressed context summary from the most recent compaction.
    /// Persisted to the session DB so it survives across sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed_summary: Option<String>,
    /// (A): raw turns that the most recent compression
    /// folded into `compressed_summary`. Persisted to the
    /// `compression_archives` table by
    /// `SessionPersistence::archive_compressed_turns` so the original
    /// wording survives even after `flush_messages` wipes those rows
    /// from `messages`. `None` when no compression has run this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_archived_turns: Option<Vec<Message>>,
    /// (B): metrics from the most recent compression —
    /// original/compressed turns + tokens + savings. Persisted to
    /// `compression_summary_history` so the full compaction arc is
    /// queryable (the `sessions.compressed_summary` column only holds
    /// the latest).
    /// Field-for-field mirror of
    /// `hermix_agent::compression::CompressionMetrics`. We can't
    /// reference the upstream type directly because `hermix-core`
    /// must not depend on `hermix-agent` (the dep arrow is the other
    /// way), and a wrapper type lets `AgentResult` stay in this
    /// crate without dragging in the compressor implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_metrics: Option<CompressionMetricsSnapshot>,
}

/// (B): field-for-field mirror of
/// `hermix_agent::compression::CompressionMetrics`. Lives in
/// `hermix-core` so `AgentResult` can carry compression metrics
/// without creating a circular `hermix-core` → `hermix-agent`
/// dependency.
/// `#[must_use]` because the snapshot is what the persistence path
/// writes to `compression_summary_history`; dropping it silently
/// loses the compaction trail that the dashboard / next-session
/// recall rely on.
#[must_use = "CompressionMetricsSnapshot feeds compression_summary_history persistence; dropping it loses the compaction trail"]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompressionMetricsSnapshot {
    pub original_tokens: u64,
    pub compressed_tokens: u64,
    pub tokens_saved: u64,
    pub compression_ratio: f64,
    pub original_turns: usize,
    pub compressed_turns: usize,
    pub turns_removed: usize,
    pub tool_results_pruned: usize,
    pub deduplication_count: usize,
    pub summary_api_calls: u32,
    pub summary_errors: u32,
    pub anti_thrashing_triggered: bool,
}

// ---------------------------------------------------------------------------
// BudgetConfig
// ---------------------------------------------------------------------------

/// Budget configuration for constraining output sizes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetConfig {
    pub max_result_size_chars: usize,
    pub max_aggregate_chars: usize,
}

/// Hard upper bound for any single budget value (per-result or aggregate).
/// `100 MiB` of UTF-8 characters ≈ ~25M GPT tokens — that's the practical
/// maximum anyone should ever need. A budget above this is almost certainly
/// a configuration typo (off-by-thousand, off-by-million) and would push
/// the loop into a memory-inefficient zone even when not actively used.
pub const BUDGET_CONFIG_MAX_CHARS: usize = 100 * 1024 * 1024;

/// Errors emitted by [`BudgetConfig::validate`]. Intentionally small:
/// both fields are `usize`, so the only sane validation is "must be
/// non-zero" and "must be below the hard ceiling".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetConfigError {
    /// `max_result_size_chars == 0` — every tool result would be
    /// silently truncated to empty.
    ZeroMaxResultSize,
    /// `max_aggregate_chars == 0` — the proportional aggregate
    /// truncation in `enforce_budget` would divide by zero.
    ZeroMaxAggregate,
    /// Either field exceeds [`BUDGET_CONFIG_MAX_CHARS`]. Almost always
    /// a unit-typo in user config.
    ExceedsCeiling { field: &'static str, value: usize },
}

impl std::fmt::Display for BudgetConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetConfigError::ZeroMaxResultSize => f.write_str(
                "BudgetConfig::max_result_size_chars must be > 0 \
                 (every tool result would be silently truncated to empty)",
            ),
            BudgetConfigError::ZeroMaxAggregate => f.write_str(
                "BudgetConfig::max_aggregate_chars must be > 0 \
                 (enforce_budget divides by this and would panic)",
            ),
            BudgetConfigError::ExceedsCeiling { field, value } => write!(
                f,
                "BudgetConfig::{field} = {value} exceeds the {BUDGET_CONFIG_MAX_CHARS}-char ceiling; \
                 this is almost certainly a unit typo (B/KiB/MB) in the user config"
            ),
        }
    }
}

impl std::error::Error for BudgetConfigError {}

impl BudgetConfig {
    /// Validate that the budget is usable. Run this once at startup —
    /// the agent loop assumes both fields are positive and bounded,
    /// and silently producing empty tool outputs / NaN ratios would
    /// be much harder to debug than a clear failure.
    pub fn validate(&self) -> Result<(), BudgetConfigError> {
        if self.max_result_size_chars == 0 {
            return Err(BudgetConfigError::ZeroMaxResultSize);
        }
        if self.max_aggregate_chars == 0 {
            return Err(BudgetConfigError::ZeroMaxAggregate);
        }
        if self.max_result_size_chars > BUDGET_CONFIG_MAX_CHARS {
            return Err(BudgetConfigError::ExceedsCeiling {
                field: "max_result_size_chars",
                value: self.max_result_size_chars,
            });
        }
        if self.max_aggregate_chars > BUDGET_CONFIG_MAX_CHARS {
            return Err(BudgetConfigError::ExceedsCeiling {
                field: "max_aggregate_chars",
                value: self.max_aggregate_chars,
            });
        }
        Ok(())
    }

    /// Same as [`Self::validate`] but logs `warn` instead of returning
    /// Err. Useful for telemetry callbacks that prefer not to block
    /// startup on a soft misconfiguration.
    pub fn validate_or_warn(&self) {
        if let Err(e) = self.validate() {
            tracing::warn!(
                target: "hermix.core",
                error = %e,
                "BudgetConfig failed validation — using as-is (caller did not propagate error)"
            );
        }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 1_000_000,
        }
    }
}

// ---------------------------------------------------------------------------
// LlmResponse
// ---------------------------------------------------------------------------

/// Complete response from an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmResponse {
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Streaming types
// ---------------------------------------------------------------------------

/// Delta for a function call within a streaming tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCallDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Delta for a tool call within a streaming response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionCallDelta>,
}

/// Delta content in a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// A single chunk from a streaming LLM response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<StreamDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

/// Session-level context window usage snapshot. Persisted per session and
/// returned in `SessionMessagesResponse` so the frontend can show the
/// context-usage bar immediately after restoring an old session, without
/// waiting for a fresh live stream to emit a `context_usage` event.
/// Mirrors the wire shape of the live `context_usage` WS event
/// (`used_tokens` / `max_tokens` / `threshold_tokens` / `percent`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextUsagePayload {
    pub used_tokens: u64,
    pub max_tokens: u64,
    pub threshold_tokens: u64,
    /// Percent of `max_tokens` currently consumed (0–100).
    pub percent: f64,
}

// ---------------------------------------------------------------------------
//Orchestration Engine types
// ---------------------------------------------------------------------------

/// Working mode for a Task. Determines whether the engine blocks for user
/// confirmation before executing (Plan), runs straight through (Craft), or
/// only answers questions without writing (Ask).
/// Maps to the WorkBuddy mode state machine: the mode is set at Task
/// creation time and governs the lifecycle transitions available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskMode {
    /// Direct execution — the agent runs tools immediately, no plan
    /// confirmation step. Suitable for short, well-defined tasks.
    #[default]
    Craft,
    /// Plan-then-execute — the agent first produces a plan, pauses for
    /// user confirmation, then executes. The Task enters `planning`
    /// status before transitioning to `in_progress`.
    Plan,
    /// Read-only advisory — the agent may read files and answer
    /// questions but MUST NOT execute write-class tools (write_file,
    /// patch, terminal, etc.). Enforced by the mode gate in the
    /// orchestrator.
    Ask,
}

/// Lifecycle status for a Task. The transitions are:
/// ```text
/// pending → planning → in_progress → completed
/// ↘ failed
/// any → archived
/// ```
/// `Planning` is only entered when `mode == Plan`. `Craft` goes straight
/// from `pending` to `in_progress`. `Ask` never enters `in_progress`
/// (the task is answered, not executed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Planning,
    InProgress,
    Completed,
    Failed,
    Archived,
}

impl TaskStatus {
    /// Returns `true` if the task is in a terminal state (no further
    /// transitions except `Archived`).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Archived)
    }

    /// Returns `true` if the task is currently being actively worked on.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Planning | Self::InProgress)
    }

    /// Validate that a transition from `self` to `next` is legal per
    /// the state machine. Returns `Ok(())` on success, `Err` with a
    /// human-readable message on illegal transition.
    pub fn validate_transition(&self, next: TaskStatus) -> Result<(), String> {
        use TaskStatus::*;
        let legal = matches!(
            (self, next),
            (Pending, Planning)
                | (Pending, InProgress)
                | (Pending, Completed)
                | (Pending, Archived)
                | (Planning, InProgress)
                | (Planning, Completed)
                | (Planning, Archived)
                | (Planning, Failed)
                | (InProgress, Completed)
                | (InProgress, Failed)
                | (InProgress, Archived)
                | (Completed, Archived)
                | (Failed, Archived)
                | (Archived, Archived) // idempotent
        );
        if legal {
            Ok(())
        } else {
            Err(format!(
                "illegal task status transition: {:?} → {:?}",
                self, next
            ))
        }
    }
}

/// Context attached to a Task — references to files, screenshots,
/// uploaded documents, and free-form notes that the agent should
/// consider when working on this Task.
/// Serialized as `context_json` in the `tasks` table. The `@`-reference
/// parser populates `references` from the user's natural-language
/// input.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TaskContext {
    /// `@`-references resolved from user input (file paths, URLs, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<TaskContextRef>,
    /// Screenshot image paths (absolute or relative to workspace).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<String>,
    /// Uploaded file paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// Free-form notes the user typed alongside the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A single `@`-reference in a Task's context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskContextRef {
    /// The raw text after `@` (e.g. `@src/main.rs` → `src/main.rs`).
    pub target: String,
    /// What kind of reference was resolved.
    pub kind: TaskContextRefKind,
}

/// Classification of a `@`-reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskContextRefKind {
    File,
    Directory,
    Url,
    Session,
    Unknown,
}

/// A reference to a deliverable artifact produced by the agent for a Task.
/// Collected by the `present_files` equivalent and stored as
/// `result_refs_json` in the `tasks` table. The frontend renders these
/// in the "产物" tab of the results panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResultRef {
    /// Absolute path or URL to the artifact.
    pub path: String,
    /// Artifact type — drives the preview rendering.
    pub artifact_type: TaskArtifactType,
    /// Optional one-line summary extracted from the tool output or
    /// generated by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Classification of a deliverable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskArtifactType {
    Html,
    Image,
    Report,
    Video,
    File,
    Directory,
}

// ---------------------------------------------------------------------------
// Skill and Memory types (referenced by trait definitions)
// ---------------------------------------------------------------------------

/// Metadata for a skill listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMeta {
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

/// A skill definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// CommandOutput (referenced by TerminalBackend trait)
// ---------------------------------------------------------------------------

/// Output from a terminal command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_construction() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content.as_deref(), Some("Hello"));

        let msg = Message::system("You are helpful");
        assert_eq!(msg.role, MessageRole::System);
    }

    #[test]
    fn test_tool_result() {
        let ok = ToolResult::ok("call_1", "result text");
        assert!(!ok.is_error);

        let err = ToolResult::err("call_2", "failed");
        assert!(err.is_error);
    }

    #[test]
    fn test_reasoning_content_from_string() {
        let val = serde_json::Value::String("thinking...".to_string());
        let rc = ReasoningContent::from_value(&val).unwrap();
        assert_eq!(rc.text, "thinking...");
        assert_eq!(rc.format, ReasoningFormat::Simple);
    }

    #[test]
    fn test_reasoning_content_from_array() {
        let val = serde_json::json!([
            {"text": "step 1"},
            {"text": "step 2"}
        ]);
        let rc = ReasoningContent::from_value(&val).unwrap();
        assert_eq!(rc.text, "step 1\nstep 2");
        assert_eq!(rc.format, ReasoningFormat::Details);
    }

    #[test]
    fn test_serde_message_role() {
        let json = serde_json::to_string(&MessageRole::Assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
        let role: MessageRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, MessageRole::Assistant);
    }

    #[test]
    fn test_budget_config_default() {
        let bc = BudgetConfig::default();
        assert_eq!(bc.max_result_size_chars, 100_000);
        assert_eq!(bc.max_aggregate_chars, 1_000_000);
    }

    #[test]
    fn test_budget_config_validate_ok_on_default() {
        BudgetConfig::default().validate().expect("default must validate");
    }

    #[test]
    fn test_budget_config_validate_zero_max_result() {
        let bc = BudgetConfig {
            max_result_size_chars: 0,
            max_aggregate_chars: 1_000_000,
        };
        assert_eq!(bc.validate(), Err(BudgetConfigError::ZeroMaxResultSize));
    }

    #[test]
    fn test_budget_config_validate_zero_max_aggregate() {
        let bc = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 0,
        };
        assert_eq!(bc.validate(), Err(BudgetConfigError::ZeroMaxAggregate));
    }

    #[test]
    fn test_budget_config_validate_exceeds_ceiling_max_result() {
        let bc = BudgetConfig {
            max_result_size_chars: BUDGET_CONFIG_MAX_CHARS + 1,
            max_aggregate_chars: 1_000_000,
        };
        assert_eq!(
            bc.validate(),
            Err(BudgetConfigError::ExceedsCeiling {
                field: "max_result_size_chars",
                value: BUDGET_CONFIG_MAX_CHARS + 1,
            })
        );
    }

    #[test]
    fn test_budget_config_validate_exceeds_ceiling_max_aggregate() {
        let bc = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: BUDGET_CONFIG_MAX_CHARS + 1,
        };
        assert_eq!(
            bc.validate(),
            Err(BudgetConfigError::ExceedsCeiling {
                field: "max_aggregate_chars",
                value: BUDGET_CONFIG_MAX_CHARS + 1,
            })
        );
    }

    /// Aggregate *less than* per-result is allowed (the proportional
    /// truncation in `enforce_budget` divides by total and re-applies
    /// the ratio to each). Validation must NOT reject this asymmetry.
    #[test]
    fn test_budget_config_validate_allows_aggregate_smaller_than_per_result() {
        let bc = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 200,
        };
        bc.validate().expect("aggregate < per_result is a valid config");
    }

    #[test]
    fn test_budget_config_validate_or_warn_does_not_panic_on_default() {
        BudgetConfig::default().validate_or_warn();
    }

    //── Orchestration Engine types ──────────────────

    #[test]
    fn test_task_mode_serde() {
        let json = serde_json::to_string(&TaskMode::Craft).unwrap();
        assert_eq!(json, "\"craft\"");
        let json = serde_json::to_string(&TaskMode::Plan).unwrap();
        assert_eq!(json, "\"plan\"");
        let json = serde_json::to_string(&TaskMode::Ask).unwrap();
        assert_eq!(json, "\"ask\"");
    }

    #[test]
    fn test_task_status_serde() {
        let json = serde_json::to_string(&TaskStatus::InProgress).unwrap();
        assert_eq!(json, "\"in_progress\"");
        let s: TaskStatus = serde_json::from_str("\"completed\"").unwrap();
        assert_eq!(s, TaskStatus::Completed);
    }

    #[test]
    fn test_task_status_default_is_pending() {
        assert_eq!(TaskStatus::default(), TaskStatus::Pending);
    }

    #[test]
    fn test_task_status_is_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Archived.is_terminal());
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::InProgress.is_terminal());
    }

    #[test]
    fn test_task_status_validate_transition_legal() {
        assert!(TaskStatus::Pending
            .validate_transition(TaskStatus::Planning)
            .is_ok());
        assert!(TaskStatus::Pending
            .validate_transition(TaskStatus::InProgress)
            .is_ok());
        assert!(TaskStatus::Planning
            .validate_transition(TaskStatus::InProgress)
            .is_ok());
        assert!(TaskStatus::InProgress
            .validate_transition(TaskStatus::Completed)
            .is_ok());
        assert!(TaskStatus::InProgress
            .validate_transition(TaskStatus::Failed)
            .is_ok());
        assert!(TaskStatus::Completed
            .validate_transition(TaskStatus::Archived)
            .is_ok());
        assert!(TaskStatus::Archived
            .validate_transition(TaskStatus::Archived)
            .is_ok());
    }

    #[test]
    fn test_task_status_validate_transition_illegal() {
        assert!(TaskStatus::Completed
            .validate_transition(TaskStatus::InProgress)
            .is_err());
        assert!(TaskStatus::Failed
            .validate_transition(TaskStatus::InProgress)
            .is_err());
        assert!(TaskStatus::Archived
            .validate_transition(TaskStatus::InProgress)
            .is_err());
        // Pending → Completed is now LEGAL (P0 automation
        // flow: automation creates a Pending task, fires a NoopAgent
        // which returns Ok immediately, the trigger marks the task
        // Completed without ever going through InProgress). The
        // test now pins Pending → Failed as still illegal (no work
        // was attempted, can't fail without trying).
        assert!(TaskStatus::Pending
            .validate_transition(TaskStatus::Failed)
            .is_err());
    }

    #[test]
    fn test_task_context_serde_roundtrip() {
        let ctx = TaskContext {
            references: vec![TaskContextRef {
                target: "src/main.rs".to_string(),
                kind: TaskContextRefKind::File,
            }],
            screenshots: vec!["screenshot1.png".to_string()],
            attachments: vec![],
            notes: Some("Fix the bug".to_string()),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: TaskContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    #[test]
    fn test_task_context_default_empty() {
        let ctx = TaskContext::default();
        assert!(ctx.references.is_empty());
        assert!(ctx.screenshots.is_empty());
        assert!(ctx.attachments.is_empty());
        assert!(ctx.notes.is_none());
    }

    #[test]
    fn test_task_result_ref_serde() {
        let r = TaskResultRef {
            path: "/output/report.html".to_string(),
            artifact_type: TaskArtifactType::Html,
            summary: Some("Generated report".to_string()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: TaskResultRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
