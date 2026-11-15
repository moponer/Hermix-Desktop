//! 端到端：一次成功的多工具会话必须在收尾时沉淀出 auto skill。
//!
//! 这是 `AgentLoop::maybe_write_auto_skill`（`agent_loop/config.rs`）的接线
//! 证明。在那之前 `skill_auto_create::{should_auto_create_skill,
//! write_auto_skill}` 是**写好但从未被调用**的死代码——"总结经验为 skill"
//! 因此完全依赖模型自愿调 `skill_manage`，用户明确要求沉淀时经常什么都没有。
//!
//! 断言落在文件系统上（SKILL.md 真的存在且内容可读），因为那才是"下次会话
//! 能复用"的唯一证据。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use hermix_agent::agent_loop::ToolRegistry;
use hermix_agent::{AgentConfig, AgentLoop};
use hermix_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall,
    ToolSchema,
};

/// 三轮：先 3 个工具调用，再 3 个，最后纯文本收尾。
/// 6 次调用 / 2 种工具 / 主导占比 50% —— 越过 `should_auto_create_skill`
/// 的全部三道门（≥5 次、≥2 种、主导 ≤80%）。
struct SequenceProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for SequenceProvider {
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
        let call = |name: &str, id: &str, args: &str| ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        };
        let (msg, finish) = match n {
            0 => (
                Message::assistant_with_tool_calls(
                    Some("开始".to_string()),
                    vec![
                        call("read_file", "c1", r#"{"path":"a.txt"}"#),
                        call("terminal", "c2", r#"{"command":"dir"}"#),
                        call("read_file", "c3", r#"{"path":"b.txt"}"#),
                    ],
                ),
                "tool_calls",
            ),
            1 => (
                Message::assistant_with_tool_calls(
                    Some("继续".to_string()),
                    vec![
                        call("terminal", "c4", r#"{"command":"type a.txt"}"#),
                        call("read_file", "c5", r#"{"path":"c.txt"}"#),
                        call("terminal", "c6", r#"{"command":"echo done"}"#),
                    ],
                ),
                "tool_calls",
            ),
            _ => {
                let saw_all = messages
                    .iter()
                    .filter(|m| m.tool_call_id.is_some())
                    .count();
                (
                    Message::assistant(if saw_all >= 6 { "done" } else { "incomplete" }),
                    "stop",
                )
            }
        };
        Ok(LlmResponse {
            message: msg,
            usage: None,
            model: "mock:model".to_string(),
            finish_reason: Some(finish.to_string()),
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

fn registry() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    let schema = |name: &str| ToolSchema {
        name: name.to_string(),
        description: name.to_string(),
        parameters: JsonSchema::new("object"),
    };
    for name in ["read_file", "terminal"] {
        tools.register(name, schema(name), Arc::new(|p| Ok(p.to_string())));
    }
    tools
}

/// Harness² 任务级递归的端到端证明：**两次同任务执行必须产出对比版技能**。
///
/// 第一次 run 故意永不收敛（每轮都发同一个工具调用，`max_turns` 耗尽 →
/// `finished_naturally=false` → judge 判 Failed）；第二次 run 正常成功。两次的
/// 任务原文相同 → 同一签名 → Compose 对比出"失败路径独有 `list_files`"这条
/// 分歧点，写到 `skills/auto/probe-<sig>/SKILL.md`。
#[tokio::test]
async fn two_runs_of_the_same_task_compose_a_contrastive_skill() {
    /// 永不返回 stop：每轮都发 `list_files`。
    struct NeverFinishes;
    #[async_trait::async_trait]
    impl LlmProvider for NeverFinishes {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    Some("still going".to_string()),
                    vec![ToolCall {
                        id: "loop-1".to_string(),
                        function: FunctionCall {
                            name: "list_files".to_string(),
                            arguments: r#"{"pattern":"*"}"#.to_string(),
                        },
                    }],
                ),
                usage: None,
                model: "mock".to_string(),
                finish_reason: Some("tool_calls".to_string()),
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

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_string_lossy().to_string();
    let task = "把这三个文件整理并汇报";

    let mut tools = ToolRegistry::new();
    let schema = |name: &str| ToolSchema {
        name: name.to_string(),
        description: name.to_string(),
        parameters: JsonSchema::new("object"),
    };
    for name in ["read_file", "terminal", "list_files"] {
        tools.register(name, schema(name), Arc::new(|p| Ok(p.to_string())));
    }
    let tools = Arc::new(tools);

    // Run 1：失败（耗尽轮次）。
    let failing = AgentLoop::new(
        AgentConfig {
            max_turns: 3,
            hermix_home: Some(home.clone()),
            ..AgentConfig::default()
        },
        tools.clone(),
        Arc::new(NeverFinishes),
    );
    let first = failing.run(vec![Message::user(task)], None).await.unwrap();
    assert!(!first.finished_naturally, "第一次必须是不收敛结束");

    let probes = std::path::Path::new(&home).join("probes");
    let sig_files: Vec<_> = std::fs::read_dir(&probes)
        .expect("探针目录必须被创建")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(sig_files.len(), 1, "同一任务只落一个签名文件");

    // Run 2：成功（2 个不同工具后收尾）。
    let succeeding = AgentLoop::new(
        AgentConfig {
            max_turns: 10,
            hermix_home: Some(home.clone()),
            ..AgentConfig::default()
        },
        tools,
        Arc::new(SequenceProvider {
            calls: AtomicUsize::new(0),
        }),
    );
    let second = succeeding.run(vec![Message::user(task)], None).await.unwrap();
    assert!(second.finished_naturally);

    // 探针必须累积到 2 条（同一签名）。
    let sig_path = sig_files[0].path();
    let records = std::fs::read_to_string(&sig_path).unwrap();
    assert_eq!(
        records.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "两次执行必须记进同一份探针历史: {records}"
    );

    // Compose 的产物必须落盘，且内容是对比（有效路径 + 分歧点）。
    let composed_dir = std::path::Path::new(&home)
        .join("skills")
        .join("auto");
    let mut found = None;
    for entry in std::fs::read_dir(&composed_dir).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("probe-") {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(entry.path().join("SKILL.md")) {
            found = Some((name, body));
        }
    }
    let (dir_name, body) = found.expect("两次执行后必须产出对比版技能");
    assert!(dir_name.starts_with("probe-"), "{dir_name}");
    assert!(body.contains("有效的路径"), "必须给出走通的那条路: {body}");
    assert!(
        body.contains("list_files"),
        "失败路径独有的工具必须成为分歧点: {body}"
    );
    assert!(body.contains("执行统计"), "必须给出可核验的统计: {body}");
}

#[tokio::test]
async fn successful_multi_tool_run_writes_an_auto_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_string_lossy().to_string();

    let loop_engine = AgentLoop::new(
        AgentConfig {
            max_turns: 10,
            hermix_home: Some(home.clone()),
            ..AgentConfig::default()
        },
        Arc::new(registry()),
        Arc::new(SequenceProvider {
            calls: AtomicUsize::new(0),
        }),
    );

    let result = loop_engine
        .run(vec![Message::user("整理这三个文件并汇报")], None)
        .await
        .expect("loop should succeed");

    assert_eq!(
        result.messages.iter().rev().find_map(|m| m.content.clone()).as_deref(),
        Some("done"),
        "6 个工具结果都必须回到上下文，否则沉淀的序列是残缺的"
    );

    // auto skill 必须落盘，且内容是可复用的操作手册。
    let auto_dir = std::path::Path::new(&home).join("skills").join("auto");
    let written = std::fs::read_dir(&auto_dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .flat_map(|e| {
                    let p = e.path().join("SKILL.md");
                    std::fs::read_to_string(p).ok()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(written.len(), 1, "恰好沉淀一个 auto skill: {auto_dir:?}");
    let body = &written[0];
    assert!(body.contains("整理这三个文件并汇报"), "任务原文要进正文");
    assert!(body.contains("1. `read_file`"), "步骤要按顺序编号: {body}");
    assert!(body.contains("2. `terminal`"), "{body}");
    assert!(body.contains("6. `terminal`"), "{body}");
    assert!(body.contains("kind: auto"), "frontmatter 要标明来源: {body}");
    // 中文任务名过不了 ASCII slug 规则，必须退化到工具序列命名而不是 "auto"，
    // 否则每个中文任务都撞同一个名字、从第二个起静默丢失。
    let dir_name = std::fs::read_dir(&auto_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .next()
        .unwrap();
    assert_eq!(dir_name, "read-file-terminal", "slug 应来自工具序列: {dir_name}");
}

/// 反向哨兵：只调一种工具的会话不该沉淀——那不是"流程"，是重复重试。
#[tokio::test]
async fn single_tool_run_does_not_write_a_skill() {
    struct OneTool;
    #[async_trait::async_trait]
    impl LlmProvider for OneTool {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "mock".to_string(),
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

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_string_lossy().to_string();
    let loop_engine = AgentLoop::new(
        AgentConfig {
            max_turns: 5,
            hermix_home: Some(home.clone()),
            ..AgentConfig::default()
        },
        Arc::new(registry()),
        Arc::new(OneTool),
    );
    let _ = loop_engine
        .run(vec![Message::user("hi")], None)
        .await
        .expect("loop should succeed");

    assert!(
        !std::path::Path::new(&home).join("skills").join("auto").exists()
            || std::fs::read_dir(std::path::Path::new(&home).join("skills").join("auto"))
                .map(|rd| rd.count())
                .unwrap_or(0)
                == 0,
        "没有工具调用的会话不该产出 skill"
    );
}
