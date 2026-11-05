//! Message-and-callback helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `emit_status` / `emit_compaction_status` — fire callback notifications.
//! - `assistant_visible_text` / `assistant_visible_text_after_think_blocks` /
//!   `assistant_has_reasoning` — content predicates.
//! - `normalize_tool_call_arguments` — argument JSON sanity check.
//! - `extra_body_for_api_mode` — derive the API-mode-specific extra-body map.

use hermix_core::{Message, ToolCall};
use serde_json::Value;

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::python_alignment::strip_think_blocks_for_ack;
use crate::smart_model_routing::ApiMode;

/// Fire a status callback, honoring `quiet_mode`.
pub fn emit_status(
    config: &AgentConfig,
    callbacks: &AgentCallbacks,
    event_type: &str,
    message: &str,
) {
    if config.quiet_mode {
        return;
    }
    if let Some(cb) = callbacks.status_callback.as_ref() {
        cb(event_type, message);
    }
}

/// forward a `BreakerSnapshot` to whoever wired
/// `AgentCallbacks::on_compaction_status`. The snapshot already
/// carries every field the front-end `CompactionStatusBanner`
/// needs (state, pressure, failure reason, change timestamp)
/// so the upstream layer just JSON-serialises it and emits the
/// `compaction_status` WS event.
/// `quiet_mode` does NOT short-circuit this path: the banner is
/// a UI affordance, and suppressing it would hide legitimate
/// "context is overflowing" / "compression has been disabled"
/// signals from the user.
pub fn emit_compaction_status(
    callbacks: &AgentCallbacks,
    snapshot: &hermix_core::CompressionEngineSnapshot,
) {
    if let Some(cb) = callbacks.on_compaction_status.as_ref() {
        let crate_snapshot = crate::compression::BreakerSnapshot {
            state: match snapshot.state {
                hermix_core::CompressionEngineState::Closed => crate::compression::BreakerState::Closed,
                hermix_core::CompressionEngineState::HalfOpen => crate::compression::BreakerState::HalfOpen,
                hermix_core::CompressionEngineState::Open => crate::compression::BreakerState::Open,
            },
            used_tokens: snapshot.current_tokens,
            threshold_tokens: snapshot.threshold_tokens,
            max_tokens: snapshot.max_tokens,
            last_failure_reason: snapshot.failure_reason.clone(),
            // forward the compression-process phase so the
            // front-end `CompactionStatusBanner` can render
            // "正在准备压缩" → "正在压缩中" → "压缩完成" / "压缩失败"
            // transitions instead of jumping straight from the
            // pressure warning to the post-attempt result.
            phase: snapshot.phase.clone(),
            ..Default::default()
        };
        cb(&crate_snapshot);
    }
}

/// True when the message has a non-empty `content` field.
pub fn assistant_visible_text(m: &Message) -> bool {
    m.content
        .as_deref()
        .map(str::trim)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// True when the message has visible text after stripping
/// `<think>...</think>` blocks (used by the ACK pipeline).
pub fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
    let Some(content) = m.content.as_deref() else {
        return false;
    };
    !strip_think_blocks_for_ack(content).trim().is_empty()
}

/// True when the message carries a non-empty `reasoning_content` block.
pub fn assistant_has_reasoning(m: &Message) -> bool {
    m.reasoning_content
        .as_deref()
        .map(str::trim)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// True when the assistant produced anything the user can actually read this
/// turn: body text after stripping `<think>` blocks, **or** a reasoning block.
///
/// 收敛判定必须用这个口径而不是只认 `content`。thinking 模型（MiniMax
/// M3.1-Flash 等）把推演写在 `reasoning_content` 里、正文留空，而前端是把
/// 这段思考原样渲染给用户看的。只认正文等于把「用户正在读它的思考」记成
/// 「它什么都没说」——连续/累计 18 轮就触发 `NoProgressWithTools` 硬停，
/// 合法长任务被砍在半路（2026-11-05 实测：22 轮的课程目录探查硬停）。
///
/// 真正的空转由 `RepeatGuard`（同一批工具结果逐字重复）兜底，那是「零新
/// 信息」的硬证据；「没写正文」不是。
pub fn assistant_produced_output(m: &Message) -> bool {
    assistant_visible_text_after_think_blocks(m) || assistant_has_reasoning(m)
}

/// Coerce empty arguments to `"{}"` and verify the rest is valid JSON.
/// Returns an error if the arguments string is non-empty and unparseable.
pub fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
    let trimmed = tc.function.arguments.trim();
    if trimmed.is_empty() {
        tc.function.arguments = "{}".to_string();
        return Ok(());
    }
    serde_json::from_str::<Value>(trimmed)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Derive the `extra_body` map for the active API mode.
/// - `CodexResponses` mode passes the user-supplied body through unchanged.
/// - For every other mode, inject `strict_api=true` unless the user
/// already set `strict_tool_calls`, `strict_api`, or `provider_strict`.
/// - Returns `None` when the user supplied a non-object `extra_body`
/// (the upstream provider would just reject it anyway).
pub fn extra_body_for_api_mode(config: &AgentConfig, api_mode: &ApiMode) -> Option<Value> {
    let mut body = config
        .extra_body
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    if !body.is_object() {
        return config.extra_body.clone();
    }
    if !matches!(api_mode, ApiMode::CodexResponses)
        && body.get("strict_tool_calls").is_none()
        && body.get("strict_api").is_none()
        && body.get("provider_strict").is_none()
    {
        body["strict_api"] = Value::Bool(true);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_only_turn_counts_as_output() {
        let mut m = Message::assistant("");
        m.reasoning_content = Some("catalog is lazily rendered; scroll .left".into());
        assert!(!assistant_visible_text_after_think_blocks(&m));
        assert!(
            assistant_produced_output(&m),
            "reasoning is rendered to the user, so it is output"
        );
    }

    #[test]
    fn empty_body_and_blank_reasoning_is_not_output() {
        // `<think>` 块不算正文（它由 reasoning 通道单独承载）。
        let mut m = Message::assistant("<think>内部推演</think>");
        assert!(!assistant_produced_output(&m));

        m.reasoning_content = Some("   \n ".into());
        assert!(!assistant_produced_output(&m), "whitespace-only is empty");

        m.content = Some("目录只渲染了 3 节".into());
        assert!(assistant_produced_output(&m));
    }
}
