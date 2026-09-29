// agent_loop/scripted_provider.rs
//
// 脚本化 LLM provider —— transcript replay 测试的底座。
//
// 动机（2026-09-28，对标 dsh 的 `packages/test-support/llm-replay`）：
// 我们此前的 1064 个测试全是单元级——没有一个覆盖「完整 run() 的
// 端到端行为」。于是 run_loop 的循环结构改动（watchdog 布防点、工具
// 预算、审批等待）只能靠人肉回归验证。这个 provider 让 agent loop
// 可以对着**确定性脚本**跑完整条 loop→tools→persistence 链。
//
// 三条语义（与 dsh 一致，都是为了防"测试通过但行为变了"）：
//
// 1. **replay**：第 N 次调用返回脚本第 N 轮；
// 2. **underrun 失败**：脚本耗尽后返回 Err —— 绝不静默返回一个空
//    assistant 响应（那会让"agent 少跑一轮"看起来像测试通过）；
// 3. **seen 记录**：每次调用时把模型可见消息快照下来，断言的是
//    "agent 实际看到的历史"，而不是我们以为它看到的历史。
//
// 刻意不做的事：不做真实 API 的 record 模式。录制真实会话需要网络
// 与凭据，会让 CI 不自洽；脚本化 + seen 断言已经覆盖了我们要防的
// 回归类别（循环结构 / 工具链 / 审批 / 压缩触发点）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall, ToolSchema};

/// 脚本的一轮：模型这一次返回什么。
#[derive(Debug, Clone, Default)]
pub struct ScriptedTurn {
    /// `<think>` 内容（reasoning_content）。
    pub reasoning: Option<String>,
    /// 可见正文。
    pub text: String,
    /// 工具调用（与 text 同时存在时，loop 会先执行工具再进下一轮）。
    pub tool_calls: Vec<ToolCall>,
}

impl ScriptedTurn {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    pub fn with_tool(mut self, name: &str, args: &str) -> Self {
        self.tool_calls.push(ToolCall {
            id: format!("call_{}", self.tool_calls.len() + 1),
            function: hermix_core::FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        });
        self
    }
}

/// 一次 LLM 调用的模型可见输入（断言用）。
#[derive(Debug, Clone)]
pub struct SeenCall {
    /// 消息序列的紧凑投影：`role:content 前 80 字符`。
    pub message_digest: Vec<String>,
    /// 本次调用可见的工具名。
    pub tool_names: Vec<String>,
    /// 消息总条数。
    pub message_count: usize,
}

pub struct ScriptedProvider {
    remaining: Mutex<VecDeque<ScriptedTurn>>,
    seen: Mutex<Vec<SeenCall>>,
    calls: AtomicUsize,
}

impl ScriptedProvider {
    /// Build a provider that replays `turns` in order.
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            remaining: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    /// Shared handle (so tests can inspect after the loop finished).
    pub fn shared(turns: Vec<ScriptedTurn>) -> Arc<Self> {
        Arc::new(Self::new(turns))
    }

    /// Every call the loop made, in order.
    pub fn seen(&self) -> Vec<SeenCall> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn record(&self, messages: &[Message], tools: &[ToolSchema]) {
        let digest = messages
            .iter()
            .map(|m| {
                let body = m.content.clone().unwrap_or_default();
                let head: String = body.chars().take(80).collect();
                format!("{:?}:{}", m.role, head)
            })
            .collect();
        let entry = SeenCall {
            message_digest: digest,
            tool_names: tools.iter().map(|t| t.name.clone()).collect(),
            message_count: messages.len(),
        };
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(entry);
        }
    }

    /// Pop the next scripted turn. Underrun is an error on purpose.
    fn next_turn(&self) -> Result<ScriptedTurn, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.remaining
            .lock()
            .map_err(|_| AgentError::Config("scripted provider lock poisoned".into()))?
            .pop_front()
            .ok_or_else(|| {
                AgentError::Config(
                    "scripted provider: script exhausted (underrun) — the agent loop \
                     made more LLM calls than the fixture scripts. This is a test \
                     failure, not a benign end-of-stream."
                        .into(),
                )
            })
    }

    fn into_response(turn: ScriptedTurn) -> LlmResponse {
        let mut msg = Message::assistant(turn.text);
        msg.reasoning_content = turn.reasoning;
        if !turn.tool_calls.is_empty() {
            msg.tool_calls = Some(turn.tool_calls);
        }
        let has_tools = msg.tool_calls.is_some();
        LlmResponse {
            message: msg,
            usage: None,
            model: "scripted".into(),
            finish_reason: Some(if has_tools { "tool_calls" } else { "stop" }.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.record(messages, tools);
        Ok(Self::into_response(self.next_turn()?))
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        self.record(messages, tools);
        let turn = self.next_turn();
        futures::stream::once(async move {
            let turn = match turn {
                Ok(t) => t,
                Err(e) => return futures::stream::iter(vec![Err(e)]),
            };
            // Emit the shape real providers do: content deltas then a
            // terminal chunk carrying finish_reason. Splitting the text
            // in two keeps the streaming accumulator exercised (the
            // downstream Markdown throttle / scroll-follow path) without
            // depending on wall-clock timing.
            let half = turn.text.chars().count() / 2;
            let head: String = turn.text.chars().take(half).collect();
            let tail: String = turn.text.chars().skip(half).collect();
            let mut out: Vec<Result<StreamChunk, AgentError>> = Vec::new();
            if let Some(r) = turn.reasoning.clone() {
                out.push(Ok(StreamChunk {
                    delta: Some(hermix_core::StreamDelta {
                        content: Some(r),
                        tool_calls: None,
                        extra: Some(serde_json::json!({ "reasoning": true })),
                    }),
                    finish_reason: None,
                    usage: None,
                }));
            }
            for piece in [head, tail] {
                if piece.is_empty() {
                    continue;
                }
                out.push(Ok(StreamChunk {
                    delta: Some(hermix_core::StreamDelta {
                        content: Some(piece),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                }));
            }
            out.push(Ok(StreamChunk {
                delta: None,
                finish_reason: Some(
                    if turn.tool_calls.is_empty() { "stop" } else { "tool_calls" }.to_string(),
                ),
                usage: None,
            }));
            futures::stream::iter(out)
        })
        .flatten()
        .boxed()
    }
}
