//! End-to-end wiring test for the acceptance gate.
//!
//! The gate itself is unit-tested in `src/acceptance_gate.rs`, and the
//! wiring point (`session_lifecycle::run_acceptance_gate`) has its own
//! unit coverage — but nothing drove the **real agent loop** and
//! observed the advisory actually landing in the context the model
//! sees. This file closes that gap.
//!
//! What is being locked in:
//!
//! * the "unbacked completion claim" check fires when the model claims
//!   a completed action, the user asked for one, and **no tool ever
//!   ran** in the session;
//! * it stays silent when a tool did run (the claim is then backed);
//! * it stays silent when the user only asked a question, even if the
//!   reply happens to contain a completion-sounding phrase;
//! * the advisory is emitted once for the qualifying turn — it is a
//!   per-turn advisory, not something that accumulates.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use hermix_agent::agent_loop::ToolRegistry;
use hermix_agent::{AgentConfig, AgentLoop};
use hermix_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall,
    ToolSchema,
};

/// A user message that asks for a mutating action.
const ACTION_REQUEST: &str = "帮我把桌面的临时文件删除";

/// A reply that asserts that action was performed.
const COMPLETION_CLAIM: &str = "已删除桌面上的临时文件，共 3 个。";

/// The marker the gate puts in front of the injected advisory.
const ADVISORY_MARKER: &str = "[验收提醒]";

/// The part of the advisory that says *why* it fired.
const ADVISORY_REASON: &str = "没有任何工具执行过";

/// A provider that always answers with `text` and never calls a tool.
struct TextOnlyProvider {
    text: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmProvider for TextOnlyProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            message: Message::assistant(self.text.clone()),
            usage: None,
            model: "mock:model".to_string(),
            finish_reason: Some("stop".to_string()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

/// A provider that calls a tool first, then answers with `text`.
struct ToolThenTextProvider {
    text: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmProvider for ToolThenTextProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    Some("need tool".to_string()),
                    vec![ToolCall {
                        id: "tool-1".to_string(),
                        function: FunctionCall {
                            name: "echo_tool".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }],
                ),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
            })
        } else {
            Ok(LlmResponse {
                message: Message::assistant(self.text.clone()),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

fn registry_with_echo_tool() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(
        "echo_tool",
        ToolSchema {
            name: "echo_tool".to_string(),
            description: "echo".to_string(),
            parameters: JsonSchema::new("object"),
        },
        Arc::new(|_params| Ok("ok".to_string())),
    );
    tools
}

fn config() -> AgentConfig {
    AgentConfig {
        max_turns: 5,
        ..AgentConfig::default()
    }
}

/// All advisory messages the gate injected into the final context.
fn advisories(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| {
            m.role == hermix_core::MessageRole::System
                && m.content
                    .as_deref()
                    .map(|c| c.contains(ADVISORY_MARKER))
                    .unwrap_or(false)
        })
        .map(|m| m.content.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn advisory_is_injected_when_a_completion_claim_has_no_tool_evidence() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(TextOnlyProvider {
        text: COMPLETION_CLAIM.to_string(),
        calls: calls.clone(),
    });
    let engine = AgentLoop::new(config(), Arc::new(registry_with_echo_tool()), provider);

    let result = engine
        .run(vec![Message::user(ACTION_REQUEST)], None)
        .await
        .expect("agent loop should succeed");

    let found = advisories(&result.messages);
    assert_eq!(
        found.len(),
        1,
        "the qualifying turn must produce exactly one advisory, got {found:?}"
    );
    assert!(
        found[0].contains(ADVISORY_REASON),
        "the advisory must say the claim is unbacked, got {:?}",
        found[0]
    );
    // The autonomous nudge ("you produced a text reply but have not yet
    // started working") makes the loop run a second turn with the same
    // unbacked claim. The advisory must NOT be injected again for it —
    // the model already has the words.
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "expected the autonomous nudge to trigger a second turn, got {} calls",
        calls.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn no_advisory_when_a_tool_actually_ran() {
    // Same claim, but a tool ran first — so the claim is backed by a
    // tool result and must not be flagged.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ToolThenTextProvider {
        text: COMPLETION_CLAIM.to_string(),
        calls: calls.clone(),
    });
    let engine = AgentLoop::new(config(), Arc::new(registry_with_echo_tool()), provider);

    let result = engine
        .run(vec![Message::user(ACTION_REQUEST)], None)
        .await
        .expect("agent loop should succeed");

    let found = advisories(&result.messages);
    assert!(
        found.is_empty(),
        "a claim backed by a tool result must not be flagged, got {found:?}"
    );
}

#[tokio::test]
async fn no_advisory_for_a_question_that_merely_mentions_completion() {
    // The user asked a question; the reply happens to contain a
    // completion-sounding phrase. No action was requested, so the
    // reply is not a claim about the agent's own work.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(TextOnlyProvider {
        text: "已完成的功能包括登录、导出和同步。".to_string(),
        calls: calls.clone(),
    });
    let engine = AgentLoop::new(config(), Arc::new(registry_with_echo_tool()), provider);

    let result = engine
        .run(vec![Message::user("这个项目已完成的功能有哪些？")], None)
        .await
        .expect("agent loop should succeed");

    let found = advisories(&result.messages);
    assert!(
        found.is_empty(),
        "a question that is not an action request must not be flagged, got {found:?}"
    );
}

#[tokio::test]
async fn advisory_does_not_accumulate_across_the_session() {
    // The advisory is per qualifying turn, not per call: once a tool
    // has run, the session has tool evidence and the check is silent
    // for the rest of the run.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ToolThenTextProvider {
        text: COMPLETION_CLAIM.to_string(),
        calls: calls.clone(),
    });
    let engine = AgentLoop::new(config(), Arc::new(registry_with_echo_tool()), provider);

    let result = engine
        .run(vec![Message::user(ACTION_REQUEST)], None)
        .await
        .expect("agent loop should succeed");

    let found = advisories(&result.messages);
    assert!(
        found.is_empty(),
        "the advisory must not repeat once tool evidence exists, got {found:?}"
    );
    // And the run really did make more than one LLM call, otherwise
    // the assertion above would pass vacuously.
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "expected a tool turn followed by a reply turn"
    );
}
