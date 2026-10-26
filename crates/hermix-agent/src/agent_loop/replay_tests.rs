// agent_loop/replay_tests.rs
//
// 端到端 loop 测试：用 [`scripted_provider::ScriptedProvider`] 对着确定性
// 脚本跑完整条 run() 链（loop → 工具执行 → 结果回灌 → 终止）。
//
// 为什么需要（2026-09-28，对标 dsh 的 `llm-replay`）：此前的测试全是
// 单元级的——`process_post_response` 单测、`resolve_pre_dispatch` 单测
// 各自成立，但"run_loop 改动后整轮行为是否不变"没有任何东西守着。
// 这轮稳定性改造（watchdog 布防点、工具预算、审批等待）正好是循环结构
// 改动，靠人肉回归不可持续。
//
// 断言的是**模型可见的消息历史**（provider.seen()），不是内部变量 ——
// 因为"agent 实际看到了什么"才是回归会破坏的东西。

use super::*;
use crate::agent_config::AgentConfig;
use crate::agent_helpers::ToolRegistry;
use crate::agent_loop::scripted_provider::{ScriptedProvider, ScriptedTurn};
use hermix_core::{Message, ToolSchema};

/// 一个只会返回固定文本的假工具。
fn registry_with_echo_tool() -> Arc<ToolRegistry> {
    let mut reg = ToolRegistry::new();
    reg.register(
        "echo_tool",
        ToolSchema::new(
            "echo_tool",
            "test double",
            hermix_core::JsonSchema::new("object"),
        ),
        Arc::new(|params: serde_json::Value| {
            Ok(format!("echo:{}", params.get("text").and_then(|v| v.as_str()).unwrap_or("")))
        }),
    );
    Arc::new(reg)
}

/// 确定性场景的基线配置。
///
/// `autonomous_mode: false` 是刻意的：默认的自治模式在「模型只回文字、
/// 一轮没调过工具」时会先发一次 nudge 再收尾（设计行为，见 run_loop 的
/// `should_autonomous_nudge`），那会让脚本化断言的调用次数依赖 nudge。
/// nudge 路径本身由下面的 `replay_autonomous_nudge_*` 单独覆盖。
/// 只保留对话消息（滤掉 system 层的 SOUL / Memory 注入）——断言的
/// 是"这一轮对话发生了什么"，不是"系统提示拼了多长"。
fn conversation(seen: &[crate::agent_loop::scripted_provider::SeenCall], call: usize) -> Vec<String> {
    seen[call]
        .message_digest
        .iter()
        .filter(|d| !d.starts_with("System:"))
        .cloned()
        .collect()
}

fn test_config() -> AgentConfig {
    AgentConfig {
        autonomous_mode: false,
        max_turns: 10,
        // 收窄 budgets 让"工具超时被正确放大"这类改动在测试里可见，
        // 同时保证单测不真的等待生产预算（terminal 600s 等）。
        ..AgentConfig::default()
    }
}

// ── 场景 1：纯文本回答，一轮结束 ────────────────────────────────
#[tokio::test]
async fn replay_single_text_turn() {
    let provider = ScriptedProvider::shared(vec![ScriptedTurn::text("已收到你的问题。")]);
    let agent = AgentLoop::new(
        test_config(),
        Arc::new(ToolRegistry::new()),
        provider.clone() as Arc<dyn hermix_core::LlmProvider>,
    );

    let result = agent.run(vec![Message::user("你好")], None).await.expect("run");

    assert!(
        result.finished_naturally,
        "single text turn should finish naturally; finish_reason={:?} interrupted={} err={:?} msgs={:?}",
        result.finish_reason, result.interrupted, result.last_error,
        result.messages.iter().map(|m| format!("{:?}:{:?}", m.role, m.content)).collect::<Vec<_>>()
    );
    assert_eq!(provider.call_count(), 1, "one assistant turn → exactly one LLM call");
    let last = result.messages.last().expect("assistant message");
    assert_eq!(last.content.as_deref(), Some("已收到你的问题。"));
}

// ── 场景 2：工具调用 → 结果回灌 → 二次回答 ──────────────────────
#[tokio::test]
async fn replay_tool_call_then_summarize() {
    let provider = ScriptedProvider::shared(vec![
        ScriptedTurn::text("我先查一下。").with_tool("echo_tool", r#"{"text":"hi"}"#),
        ScriptedTurn::text("查完了：hi。"),
    ]);
    let agent = AgentLoop::new(
            test_config(),
            registry_with_echo_tool(),
            provider.clone() as Arc<dyn hermix_core::LlmProvider>,
        );

    let result = agent.run(vec![Message::user("echo hi")], None).await.expect("run");

    assert!(result.finished_naturally);
    assert_eq!(provider.call_count(), 2, "tool call forces a second LLM turn");

    // 模型在第二次调用时必须看到：user → assistant(tool_calls) → tool result。
    let seen = provider.seen();
    assert_eq!(
        conversation(&seen, 0).len(),
        1,
        "first call's conversation is just the user message (system injections excluded)"
    );
    let second = conversation(&seen, 1);
    assert_eq!(second.len(), 3, "second call sees user + assistant(tool_calls) + tool result");
    assert!(second[0].contains("User:"), "user turn visible: {:?}", second[0]);
    assert!(second[1].contains("我先查一下"), "assistant turn with tool call is visible: {:?}", second[1]);
    assert!(
        second[2].contains("echo:hi"),
        "tool result must be fed back into the next LLM call, got {:?}",
        second[2]
    );

    // 最终消息是脚本的第二轮正文。
    let last = result.messages.last().expect("final assistant");
    assert_eq!(last.content.as_deref(), Some("查完了：hi。"));
}

// ── 场景 3：reasoning 不泄漏进可见正文 ──────────────────────────
#[tokio::test]
async fn replay_reasoning_stays_in_reasoning_field() {
    let provider = ScriptedProvider::shared(vec![ScriptedTurn::text("结论在这里")
        .with_reasoning("内部推理：先看 A 再看 B")]);
    let agent = AgentLoop::new(
        test_config(),
        Arc::new(ToolRegistry::new()),
        provider.clone() as Arc<dyn hermix_core::LlmProvider>,
    );

    let result = agent.run(vec![Message::user("想一下")], None).await.expect("run");

    let last = result.messages.last().expect("assistant");
    assert_eq!(last.content.as_deref(), Some("结论在这里"));
    let reasoning = last.reasoning_content.as_deref().unwrap_or("");
    assert!(reasoning.contains("内部推理"), "reasoning must be preserved separately");
    assert!(
        !last.content.as_deref().unwrap_or("").contains("内部推理"),
        "reasoning must not leak into visible text"
    );
}

// ── 场景 3b：自治模式对"只说不做"发一次 nudge（默认行为）─────────
#[tokio::test]
async fn replay_autonomous_nudge_fires_once_then_finalizes() {
    // 自治模式（默认开）：模型回了可见文字但一轮没调工具 → loop 发一次
    // nudge 逼它干活；模型再回一次文字后收尾。nudge 只发一次
    // （autonomous_nudge_count == 0 门槛），所以调用次数是 2 而不是 3。
    let provider = ScriptedProvider::shared(vec![
        ScriptedTurn::text("我来处理。"),
        ScriptedTurn::text("已处理完毕。"),
    ]);
    let agent = AgentLoop::new(
        AgentConfig { max_turns: 10, ..AgentConfig::default() },
        registry_with_echo_tool(),
        provider.clone() as Arc<dyn hermix_core::LlmProvider>,
    );

    // nudge 的触发条件含 `!tool_schemas.is_empty()`，而 run() 的 tools
    // 参数为 None 时该集合为空 —— 显式传 schema 才能走到 nudge 分支。
    let schemas = vec![hermix_core::ToolSchema::new(
        "echo_tool",
        "test double",
        hermix_core::JsonSchema::new("object"),
    )];
    let result = agent
        .run(vec![Message::user("做点什么")], Some(schemas))
        .await
        .expect("run");

    assert!(result.finished_naturally, "nudge round should still finalize naturally");
    assert_eq!(provider.call_count(), 2, "nudge fires exactly once, then finalize");
    assert_eq!(
        result.messages.last().and_then(|m| m.content.clone()),
        Some("已处理完毕。".to_string())
    );
}

// ── 场景 4：脚本耗尽 = 失败，不静默通过（dsh underrun 语义）────────
#[tokio::test]
async fn replay_underrun_surfaces_error_instead_of_silent_success() {
    // 脚本只有一轮，但 agent 会因工具调用需要第二轮 → 第二轮拿到 Err。
    let provider = ScriptedProvider::shared(vec![ScriptedTurn::text("第一步")
        .with_tool("echo_tool", r#"{"text":"x"}"#)]);
    let agent = AgentLoop::new(
            test_config(),
            registry_with_echo_tool(),
            provider.clone() as Arc<dyn hermix_core::LlmProvider>,
        );

    let result = agent.run(vec![Message::user("go")], None).await.expect("run returns");

    // LLM 错误走 graceful interrupt 路径：run 不 panic、不挂死，但必须
    // 标记为 interrupted —— 绝不能伪装成"正常完成"。
    assert!(!result.finished_naturally, "an exhausted script must not look like a clean finish");
    assert_eq!(provider.call_count(), 2, "both calls attempted (2nd one errors)");
}

// ── 场景 5：流式路径与非流式路径产出一致 ────────────────────────
#[tokio::test]
async fn replay_stream_matches_non_stream_final_text() {
    let scripted = || {
        vec![
            ScriptedTurn::text("正在查…").with_tool("echo_tool", r#"{"text":"y"}"#),
            ScriptedTurn::text("查完了：y。"),
        ]
    };

    // 非流式
    let p1 = ScriptedProvider::shared(scripted());
    let a1 = AgentLoop::new(test_config(), registry_with_echo_tool(), p1.clone());
    let r1 = a1.run(vec![Message::user("echo y")], None).await.expect("run");

    // 流式
    let p2 = ScriptedProvider::shared(scripted());
    let a2 = AgentLoop::new(test_config(), registry_with_echo_tool(), p2.clone());
    let chunk_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = chunk_count.clone();
    let r2 = a2
        .run_stream(
            vec![Message::user("echo y")],
            None,
            Some(Box::new(move |_c: hermix_core::StreamChunk| {
                // 只计数：断言流确实被分片送出（而非一次性 whole response）。
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
        )
        .await
        .expect("stream run");
    assert!(
        chunk_count.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "scripted provider streams content in deltas"
    );

    assert!(r1.finished_naturally);
    assert_eq!(
        r1.messages.last().and_then(|m| m.content.clone()),
        r2.messages.last().and_then(|m| m.content.clone()),
        "stream and non-stream runs must reach the same final text"
    );
}

// ── 场景 6：流式空响应降级非流式成功 ──────────────────────────────
#[tokio::test]
async fn replay_stream_empty_then_degrade_to_non_stream_succeeds() {
    // 第一次（流式）返回空响应 —— 等价于"流式断流、无有效内容"；
    // classify_post_llm_response 判 RetryEmpty，inner_attempt 递增后
    // 第二次降级为非流式（call_llm_with_retry），返回正常文本。
    let provider = ScriptedProvider::shared(vec![
        ScriptedTurn::text(""),
        ScriptedTurn::text("降级后成功。"),
    ]);
    let agent = AgentLoop::new(
        test_config(),
        Arc::new(ToolRegistry::new()),
        provider.clone() as Arc<dyn hermix_core::LlmProvider>,
    );

    let result = agent
        .run_stream(vec![Message::user("回答我")], None, None)
        .await
        .expect("stream run");

    assert!(
        result.finished_naturally,
        "degraded non-stream retry should finalize naturally; finish_reason={:?} interrupted={} err={:?}",
        result.finish_reason, result.interrupted, result.last_error
    );
    assert_eq!(
        provider.call_count(),
        2,
        "one empty stream attempt + one non-stream retry"
    );
    let last = result.messages.last().expect("final assistant");
    assert_eq!(last.content.as_deref(), Some("降级后成功。"));
}

// ── 场景 7：流式空 + 非流式也失败 → graceful_errored ──────────────
#[tokio::test]
async fn replay_stream_empty_then_non_stream_underrun_is_errored() {
    // 第一次（流式）空响应 → RetryEmpty 降级；第二次（非流式）脚本
    // 耗尽 → Err → graceful_interrupt_result_errored。断言既不 panic
    // 也不伪装成正常完成，且 last_error 透出真实原因。
    let provider = ScriptedProvider::shared(vec![ScriptedTurn::text("")]);
    let agent = AgentLoop::new(
        test_config(),
        Arc::new(ToolRegistry::new()),
        provider.clone() as Arc<dyn hermix_core::LlmProvider>,
    );

    let result = agent
        .run_stream(vec![Message::user("回答我")], None, None)
        .await
        .expect("stream run");

    assert!(
        !result.finished_naturally,
        "non-stream retry failure must not look like a clean finish"
    );
    assert_eq!(provider.call_count(), 2, "stream empty + non-stream underrun");
    assert!(
        result.last_error.is_some(),
        "backend failure must surface as last_error, got {:?}",
        result.last_error
    );
}
