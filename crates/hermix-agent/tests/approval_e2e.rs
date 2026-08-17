//! E2E test for the dangerous-tool approval chain.
//!
//! written after user-reported "允许了后面为什么不进行" type-B bug
//! (run_stream hangs after tool approved, turn=2 never starts). This test
//! wires the full approval chain:
//!
//! 1. LocalAgentService with a MockProvider that returns `tool_calls` on
//!    turn 1 and `stop` on turn 2.
//! 2. A dangerous-registered tool (`dangerous_echo`).
//! 3. An injected `approval_requester` closure that mirrors the production
//!    ApprovalRegistry semantics (creates a oneshot tx/rx, returns rx to
//!    the caller, stores tx in a HashMap keyed by (session_id, request_id)).
//! 4. An injected `on_event` closure that captures every WS event the
//!    agent loop emits, so we can assert that a `request_approval` event
//!    was emitted.
//! 5. After sending the user message, the test asserts:
//!    - MockProvider was called twice (turn 1 + turn 2)
//!    - A `request_approval` event was emitted with the right request_id
//!    - The agent loop completed successfully (no hang)
//!    - The tool was actually executed (turn 2 sees a tool message in
//!      `messages`)
//!
//! This test would have caught the type-B hang at the wire level.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use hermix_agent::local_agent_service::ProviderFactory;
use hermix_agent::session_persistence::SessionPersistence;
use hermix_agent::LocalAgentService;
use hermix_config::GatewayConfig;
use hermix_core::approval::ApprovalOutcome;
use hermix_core::traits::{AgentOverrides, AgentService, LlmProvider, PermissionLevel, RiskLevel};
use hermix_core::{
    AgentError, FunctionCall, FunctionCallDelta, JsonSchema, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolError, ToolHandler, ToolSchema,
};
use hermix_tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Mock provider — emits tool_calls on turn 1, stop on turn 2.
// ---------------------------------------------------------------------------

struct MockProvider {
    calls: AtomicUsize,
    /// Filled with the messages that turn 2 saw; lets us assert
    /// the tool result was injected into ctx before turn 2 dispatched.
    last_turn_messages: Mutex<Vec<Message>>,
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_turn_messages.lock().unwrap() = messages.to_vec();
        if n == 0 {
            // Turn 1: ask for the dangerous tool.
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    Some("need dangerous tool".to_string()),
                    vec![ToolCall {
                        id: "tool-1".to_string(),
                        function: FunctionCall {
                            name: "dangerous_echo".to_string(),
                            arguments: "{\"msg\":\"hello\"}".to_string(),
                        },
                    }],
                ),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
            })
        } else {
            // Turn 2: final answer.
            Ok(LlmResponse {
                message: Message::assistant("final-answer"),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        // Count every stream invocation (one per LLM turn). Turn 1 +
        // turn 2 => exactly 2, which the test asserts against.
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Snapshot the messages this turn saw so the test can assert that
        // turn 2 observed the dangerous tool's result injected into ctx.
        // (If the type-B hang regresses, turn 2's messages won't contain
        // the tool result and the assertion fails.)
        *self.last_turn_messages.lock().unwrap() = messages.to_vec();

        // Turn 2 is the turn that already has the tool result in its ctx.
        let saw_tool_result = messages.iter().any(|m| {
            m.role == MessageRole::Tool && m.tool_call_id.as_deref() == Some("tool-1")
        });

        let chunks: Vec<Result<StreamChunk, AgentError>> = if saw_tool_result {
            // Final answer on turn 2.
            vec![
                Ok(StreamChunk {
                    delta: Some(StreamDelta {
                        content: Some("final-answer".to_string()),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                }),
                Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                }),
            ]
        } else {
            // Dangerous tool call on turn 1.
            vec![
                Ok(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index: 0,
                            id: Some("tool-1".to_string()),
                            function: Some(FunctionCallDelta {
                                name: Some("dangerous_echo".to_string()),
                                arguments: Some("{\"msg\":\"hello\"}".to_string()),
                            }),
                        }]),
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                }),
                Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some("tool_calls".to_string()),
                    usage: None,
                }),
            ]
        };
        Box::pin(futures::stream::iter(chunks))
    }
}

// ---------------------------------------------------------------------------
// Dangerous tool handler.
// ---------------------------------------------------------------------------

struct DangerousEchoHandler;

#[async_trait]
impl ToolHandler for DangerousEchoHandler {
    async fn execute(&self, params: serde_json::Value) -> Result<String, ToolError> {
        Ok(format!(
            "dangerous_echo_ok:{}",
            params
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("none")
        ))
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "dangerous_echo".to_string(),
            description: "dangerous echo".to_string(),
            parameters: JsonSchema::new("object"),
        }
    }
}

// ---------------------------------------------------------------------------
// Stand-in for `ApprovalRegistry`. Stores pending `(session, request)` → tx
// and lets the test resolve them by calling `approve(rid)`.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeApprovalRegistry {
    pending: Mutex<HashMap<(String, String), tokio::sync::oneshot::Sender<ApprovalOutcome>>>,
    request_count: AtomicUsize,
}

impl FakeApprovalRegistry {
    fn requester(
        self: &Arc<Self>,
    ) -> Arc<dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome> + Send + Sync>
    {
        let me = Arc::clone(self);
        Arc::new(move |session_id, request_id| {
            me.request_count.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = tokio::sync::oneshot::channel();
            me.pending
                .lock()
                .unwrap()
                .insert((session_id, request_id), tx);
            rx
        })
    }

    fn approve(&self, request_id: &str) -> bool {
        // Find the (session, req) entry by request_id only. In tests we
        // only have one session, so first match wins.
        let mut pending = self.pending.lock().unwrap();
        let key = pending
            .keys()
            .find(|(_, rid)| rid == request_id)
            .cloned();
        if let Some(k) = key {
            if let Some(tx) = pending.remove(&k) {
                let _ = tx.send(ApprovalOutcome::Approved);
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Event collector — captures every on_event invocation as raw JSON.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct EventLog {
    events: Mutex<Vec<serde_json::Value>>,
}

impl EventLog {
    fn callback(
        self: &Arc<Self>,
    ) -> Arc<dyn Fn(serde_json::Value) + Send + Sync> {
        let me = Arc::clone(self);
        Arc::new(move |evt| {
            me.events.lock().unwrap().push(evt);
        })
    }

    fn find_request_approval(&self) -> Option<(String, String)> {
        let events = self.events.lock().unwrap();
        for e in events.iter() {
            let evt_type = e
                .get("type")
                .and_then(|v| v.as_str())
                .or_else(|| e.get("event").and_then(|v| v.as_str()));
            if evt_type == Some("request_approval") {
                let req_id = e
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let session_id = e
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some((session_id, req_id));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// The test.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_chain_approved_tool_runs_and_loop_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();

    // -- 1. Provider factory: hand the same MockProvider to every turn. --
    let provider = Arc::new(MockProvider {
        calls: AtomicUsize::new(0),
        last_turn_messages: Mutex::new(Vec::new()),
    });
    let provider_for_factory = Arc::clone(&provider);
    let provider_factory: ProviderFactory = Arc::new(move |_cfg, _model| {
        Arc::clone(&provider_for_factory) as Arc<dyn LlmProvider>
    });

    // -- 2. Tool registry with one Dangerous tool. --
    let tool_registry = ToolRegistry::new();
    tool_registry.register_with_risk(
        "dangerous_echo",
        "test",
        ToolSchema {
            name: "dangerous_echo".to_string(),
            description: "dangerous echo".to_string(),
            parameters: JsonSchema::new("object"),
        },
        Arc::new(DangerousEchoHandler),
        Arc::new(|| true),
        vec![],
        false,
        "dangerous echo test tool",
        "[!]",
        None,
        RiskLevel::Dangerous,
        true, // override_existing: tests may re-register
    );
    let tool_registry = Arc::new(tool_registry);
    eprintln!(
        "[test] risk_level(dangerous_echo) = {:?}",
        tool_registry.risk_level("dangerous_echo")
    );

    // -- 3. Session persistence (real, on-disk). --
    let session_persistence = Arc::new(SessionPersistence::new(&home));
    session_persistence.ensure_db().expect("ensure_db");

    // -- 4. Build LocalAgentService. --
    let config = Arc::new(GatewayConfig::default());
    let service = Arc::new(LocalAgentService::new_with_provider_factory(
        config,
        tool_registry,
        session_persistence,
        provider_factory,
    ));

    // -- 5. Inject approval_requester + on_event. --
    let registry = Arc::new(FakeApprovalRegistry::default());
    let log = Arc::new(EventLog::default());

    let overrides = AgentOverrides {
        approval_requester: Some(registry.requester()),
        on_event: Some(log.callback()),
        permission: Some(PermissionLevel::Default),
        // Pin a concrete model so `resolve_effective_model` returns
        // `Ok(...)` and routes through the `provider_factory` — without
        // this, `fallback_model` returns `None` (no `llm_providers` +
        // no env keys in the test process) and the loop bails out
        // before producing any events.
        model: Some("mock:model".to_string()),
        ..Default::default()
    };

    // -- 6. Drive the agent loop and the approval resolver concurrently. --
    let on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync> = Arc::new(|_c| {});

    // Clone the shared state the resolver needs BEFORE `overrides` is moved
    // into the agent task below.
    let registry_for_resolver = Arc::clone(&registry);
    let log_for_resolver = Arc::clone(&log);

    // Run the agent loop in a background task. It blocks on the approval
    // oneshot (rx.await) once turn 1 dispatches the dangerous tool. The
    // resolver polls for the `request_approval` event and resolves it.
    // Concurrency is mandatory here: awaiting the agent result first would
    // deadlock, because the agent cannot finish until the approval is
    // granted, and the approval cannot be granted until the event is seen.
    let agent_task = tokio::spawn({
        let svc = Arc::clone(&service);
        async move {
            svc.send_message_stream("test-session-1", "hi", overrides, on_chunk)
                .await
        }
    });

    // -- 7. Resolve the pending approval from a background resolver task. --
    let resolver = tokio::spawn(async move {
        // Wait up to 30s for the request_approval event to appear.
        for i in 0..300 {
            if let Some((sid, rid)) = log_for_resolver.find_request_approval() {
                eprintln!(
                    "[resolver] found request_approval event after {}ms: session={} request={}",
                    i * 100,
                    sid,
                    rid
                );
                let approved = registry_for_resolver.approve(&rid);
                assert!(approved, "approve() should resolve the pending tx");
                return rid;
            }
            if i % 10 == 0 {
                let evts = log_for_resolver.events.lock().unwrap();
                if !evts.is_empty() {
                    eprintln!("[resolver] events after {}ms: {:#?}", i * 100, evts);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let evts = log_for_resolver.events.lock().unwrap();
        panic!(
            "timed out waiting for request_approval event; final events: {:#?}",
            evts
        );
    });

    let rid = resolver.await.expect("resolver task panicked");
    eprintln!("[test] approved request_id={}", rid);

    // -- 8. Collect the agent result (now that approval unblocked turn 2). --
    let result = agent_task
        .await
        .expect("agent task panicked")
        .expect("agent loop should succeed after approval");
    eprintln!(
        "[test] AgentReply {{ text: {:?}, message_count: {}, finish_reason: {:?} }}",
        result.text, result.message_count, result.finish_reason
    );

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "expected exactly 2 LLM calls (turn 1 + turn 2)"
    );

    // Turn 2's messages should contain a tool message with our dangerous_echo output.
    // The tool dispatcher wraps the handler's plain-string return in a JSON
    // envelope (`{"result":"..."}`) so the LLM can parse it on the next turn,
    // so we match on substring rather than exact equality.
    let turn2_messages = provider.last_turn_messages.lock().unwrap().clone();
    let saw_tool_result = turn2_messages.iter().any(|m| {
        m.role == MessageRole::Tool
            && m.content
                .as_deref()
                .map_or(false, |c| c.contains("dangerous_echo_ok:hello"))
    });
    assert!(
        saw_tool_result,
        "turn 2 must see the dangerous_echo tool result in its messages; got: {:#?}",
        turn2_messages
    );

    // And the agent must have produced a final assistant reply.
    assert_eq!(result.text, "final-answer");
    assert_eq!(result.finish_reason.as_deref(), Some("stop"));

    // The FakeApprovalRegistry saw exactly one request.
    assert_eq!(
        registry.request_count.load(Ordering::SeqCst),
        1,
        "expected exactly one approval request, got {}",
        registry.request_count.load(Ordering::SeqCst)
    );
}