//! Property 7: max_turns is a SOFT cap — at the boundary the loop injects a
//! progress-summary system notice and keeps going; it only ends when the LLM
//! stops calling tools on its own.
//! **Validates: Requirement 3.2 (revised for the soft-cap semantics —
//! `run_loop.rs` summarizes at the boundary and resets the counter instead of
//! hard-stopping; hard stops are ErrorLoop convergence / CostGuard /
//! natural stop, see `AgentConfig::default_max_turns`).**

use proptest::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermix_agent::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use hermix_core::{
    AgentError, FunctionCall, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk,
    ToolCall, ToolError, ToolSchema, UsageStats,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Scripted LLM: mirrors a real model's two call shapes.
//
// `handle_max_iterations` calls the provider with an EMPTY tools slice to get
// the progress summary, so the mock answers those calls with plain text (a
// real model would too — a summary is not a tool call). Every other call
// returns tool_calls until the scripted budget is exhausted, after which it
// returns plain text → the loop finishes naturally.
// ---------------------------------------------------------------------------

struct ScriptedLlm {
    /// Normal (non-summarizer) calls with index <= budget return tool_calls.
    tool_call_budget: usize,
    calls: Mutex<usize>,
    observed_calls: AtomicUsize,
}

impl ScriptedLlm {
    fn new(tool_call_budget: usize) -> Self {
        Self {
            tool_call_budget,
            calls: Mutex::new(0),
            observed_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.observed_calls.fetch_add(1, Ordering::SeqCst);
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        let call_idx = *calls;
        let is_summarizer = tools.is_empty();

        if !is_summarizer && call_idx <= self.tool_call_budget {
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    Some("Let me use a tool.".to_string()),
                    vec![ToolCall {
                        id: format!("call_{call_idx}"),
                        function: FunctionCall {
                            name: "echo".to_string(),
                            arguments: r#"{"input":"test"}"#.to_string(),
                        },
                    }],
                ),
                usage: Some(UsageStats {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    estimated_cost: None,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    reasoning_tokens: 0,
                }),
                model: "mock".to_string(),
                finish_reason: Some("tool_calls".to_string()),
            })
        } else {
            Ok(LlmResponse {
                message: Message::assistant("All done.".to_string()),
                usage: Some(UsageStats {
                    prompt_tokens: 10,
                    completion_tokens: 3,
                    total_tokens: 13,
                    estimated_cost: None,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    reasoning_tokens: 0,
                }),
                model: "mock".to_string(),
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
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

// ---------------------------------------------------------------------------
// Echo handler for the tool registry
// ---------------------------------------------------------------------------

fn echo_handler(params: Value) -> Result<String, ToolError> {
    Ok(params.to_string())
}

fn make_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(
        "echo",
        hermix_core::tool_schema("echo", "Echo input", hermix_core::JsonSchema::new("object")),
        Arc::new(echo_handler),
    );
    registry
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn prop_max_turns_boundary_does_not_truncate(max_turns in 1u32..5) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Budget: enough tool-calling turns to cross the max_turns
            // boundary, plus slack so the loop demonstrably continues after
            // the reset, then a natural stop.
            let budget = max_turns as usize + 3;
            let config = AgentConfig {
                max_turns,
                ..AgentConfig::default()
            };
            let registry = Arc::new(make_registry());
            let llm = Arc::new(ScriptedLlm::new(budget));
            let agent = AgentLoop::new(config, registry, llm.clone());

            let result = agent
                .run(vec![Message::user("test")], None)
                .await
                .unwrap();

            let llm_calls = llm.observed_calls.load(Ordering::SeqCst);

            // CORE invariant: the loop did NOT stop at the max_turns boundary —
            // it made strictly more LLM calls than the cap allows.
            prop_assert!(
                llm_calls > max_turns as usize,
                "soft cap violated: loop stopped at the boundary (calls={}, max_turns={})",
                llm_calls,
                max_turns
            );
            // The run ends because the LLM stopped calling tools, not because
            // the turn budget truncated it.
            prop_assert!(result.finished_naturally, "loop should finish naturally once the LLM stops calling tools");
            // The boundary crossing injected the progress-summary notice.
            prop_assert!(
                result.messages.iter().any(|m| {
                    m.role == MessageRole::System
                        && m.content
                            .as_deref()
                            .is_some_and(|c| c.contains("Maximum conversation turns reached"))
                }),
                "progress-summary system notice should be injected at the boundary"
            );
            Ok(())
        });
    }
}
