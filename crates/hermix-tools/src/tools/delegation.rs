//! Delegation tool: spawn sub-agents with isolated contexts

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// B4-3（docs/optimization-plan.md H4）：失败重派落盘
// ---------------------------------------------------------------------------

/// 同一任务（按内容哈希键控）失败重派的上限。lynn 01 复盘实测：
/// 同一张单最多被跑 6 次，43% 的消耗浪费在重复执行上。
const MAX_DELEGATION_ATTEMPTS: u32 = 3;

fn attempts_path() -> std::path::PathBuf {
    hermix_config::paths::hermix_home().join("delegation_attempts.jsonl")
}

/// 任务内容哈希——重派同任务（即使措辞微调）共享同一计数。
fn attempt_key(task: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    task.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 读取该任务已失败的次数（jsonl 逐行解析，坏行跳过）。
/// 读取该任务已失败的次数与最近失败原因（jsonl 逐行解析，坏行跳过）。
/// 返回 (次数, 最近原因)——次数用于 3 次封顶闸，原因用于 V-2 教训注入。
fn load_failed_attempts(key: &str) -> (u32, Option<String>) {
    let Ok(raw) = std::fs::read_to_string(attempts_path()) else {
        return (0, None);
    };
    let mut max_n = 0u32;
    let mut last_reason = None;
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("hash").and_then(|h| h.as_str()) == Some(key))
        .for_each(|v| {
            let n = v.get("n").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            if n >= max_n {
                max_n = n;
                last_reason =
                    v.get("last_reason").and_then(|r| r.as_str()).map(String::from);
            }
        });
    (max_n, last_reason)
}

/// 追加一次失败记录（append，崩溃最多丢最后一行）。
fn record_failed_attempt(key: &str, reason: &str) {
    use std::io::Write;
    let (n, _) = load_failed_attempts(key);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = serde_json::json!({
        "hash": key,
        "n": n,
        "last_reason": reason.chars().take(200).collect::<String>(),
        "at": at,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(attempts_path())
    {
        let _ = writeln!(f, "{line}");
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Handler for `stop_subagent`. In production the orchestrator intercepts
/// this name before dispatch (see `tool_executor`); reaching this handler
/// means no orchestrator is attached, i.e. nothing can be running.
struct StopSubagentHandler;

#[async_trait]
impl ToolHandler for StopSubagentHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Err(ToolError::ExecutionFailed(
            "stop_subagent: no sub-agent orchestrator is attached to this agent instance — \
             nothing can be running in the background."
                .to_string(),
        ))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "stop_subagent",
            "Stop a background sub-agent spawned by delegate_task(background: true).",
            JsonSchema::object(
                [(
                    "sub_agent_id".to_string(),
                    json!({
                        "type": "string",
                        "description": "The sub_agent_id returned by delegate_task when it answered {\"status\":\"spawned\"}."
                    }),
                )]
                .into_iter()
                .collect::<IndexMap<_, _>>(),
                vec!["sub_agent_id".into()],
            ),
        )
    }
}

/// Register this tool with the registry (P1.2 migration).
pub fn register(registry: &ToolRegistry, backend: Arc<dyn DelegationBackend>) {
    let handler = Arc::new(DelegateTaskHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "delegate_task",
        "delegate_task",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Delegate a task to a sub-agent with an ISOLATED context: the sub-agent reads/analyzes on its own and only its concise conclusion (capped ~8,000 chars) returns as the tool_result. SYNCHRONOUS — blocks until done; no polling.\n\n\
         ## When to use (default: prefer this over reading in the MAIN context)\n\
         Images (2,000-8,000 tokens per image if read inline), large files (> ~50 KB / ~1500 lines), PDFs > 5 pages, multi-doc comparison, and any read you will not branch on.\n\n\
         ## When NOT to use\n\
         Images the user pasted/attached in the current message (already visible — analyze directly); tiny files you need verbatim; anything where you must branch on the raw content turn-by-turn.\n\n\
         ## Parameter discipline\n\
         Pass PATHS, never inline file contents into `task`. For 4+ images split into batches of 3-5 and issue batches in parallel. Example: task=\"Summarize this PDF and list key clauses\", context=\"Path: C:\\\\docs\\\\contract.pdf\". Roles: Explore (read-only) / Plan (read+planning) / General (default).",
        "🤝",
        None,
        RiskLevel::Caution,
        false,
    );

    // stop_subagent — companion of background delegation (ZCode TaskStop).
    let stop = Arc::new(StopSubagentHandler);
    let stop_schema = stop.schema();
    registry.register_with_risk(
        "stop_subagent",
        "delegate_task",
        stop_schema,
        stop,
        Arc::new(|| true),
        vec![],
        true,
        "Stop a background sub-agent (one spawned with delegate_task `background: true`). \
         Its lineage flips to `cancelled`; get_subagent_result then reports the terminal state. \
         Returns `not_running` (not an error) when the id already finished.",
        "🛑",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// DelegationBackend trait
// ---------------------------------------------------------------------------

/// Backend for task delegation operations.
#[async_trait]
pub trait DelegationBackend: Send + Sync {
    /// Delegate a task to a sub-agent.
    async fn delegate(
        &self,
        task: &str,
        context: Option<&str>,
        toolset: Option<&str>,
        model: Option<&str>,
        child_depth: Option<u32>,
        max_depth: Option<u32>,
        parent_budget_remaining_usd: Option<f64>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// DelegateTaskHandler
// ---------------------------------------------------------------------------

/// Tool for delegating tasks to sub-agents.
pub struct DelegateTaskHandler {
    backend: Arc<dyn DelegationBackend>,
}

impl DelegateTaskHandler {
    pub fn new(backend: Arc<dyn DelegationBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for DelegateTaskHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'task' parameter".into()))?;

        let context = params.get("context").and_then(|v| v.as_str());
        let toolset = params.get("toolset").and_then(|v| v.as_str());
        let model = params.get("model").and_then(|v| v.as_str());
        let child_depth = params
            .get("child_depth")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let parent_budget_remaining_usd = params
            .get("parent_budget_remaining_usd")
            .and_then(|v| v.as_f64());

        // B4-3：失败重派落盘——同一任务失败 ≥3 次即拒绝，并如实携带
        // 失败原因（换方法/换角色/关单），不再无限原样重跑。
        let attempt_key = attempt_key(task);
        let (failed_n, last_reason) = load_failed_attempts(&attempt_key);
        if failed_n >= MAX_DELEGATION_ATTEMPTS {
            return Err(ToolError::ExecutionFailed(format!(
                "This task already failed {failed_n} times (per content history). \
                 Change the approach, delegate to a different role, or close the \
                 task — do not re-dispatch it as-is."
            )));
        }

        // V-2（docs/optimization-plan.md H3 遗留）：失败教训注入——同任务
        // 此前失败过（未达封顶）时，把失败原因注入 context，让子代理
        // 避免重蹈覆辙（lynn 08：教训跨会话留存，同域派发时注入）。
        let context = match (failed_n, last_reason.as_deref()) {
            (n, Some(reason)) if n > 0 => Some(format!(
                "{}\n\n[失败教训] 此前该任务已有 {n} 次尝试失败，最近原因：{reason}。请调整方法避免重蹈覆辙。",
                context.unwrap_or_default()
            )),
            _ => context.map(String::from),
        };

        let result = self
            .backend
            .delegate(
                task,
                context.as_deref(),toolset,
                model,
                child_depth,
                max_depth,
                parent_budget_remaining_usd,
            )
            .await;
        if let Err(ref e) = result {
            record_failed_attempt(&attempt_key, &e.to_string());
        }
        result
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "task".into(),
            json!({
                "type": "string",
                "description": "The task description for the sub-agent. Be specific about what the child should produce and how to format it.\n\n\
                 CRITICAL — DO NOT PASTE FILE CONTENTS HERE. The `task` string IS the sub-agent's first user message — if you dump 50 KB of a PDF / log / source file into `task`, you just shift the context bloat from your context to the sub-agent's context with zero benefit. Instead: (1) describe what the sub-agent should DO, (2) point it at the path, (3) let the sub-agent use its own `read_file` (with chunks if large) or `execute_command` to read it. The sub-agent's 8,000-char result cap means large inline content will be truncated anyway."
            }),
        );
        props.insert(
            "context".into(),
            json!({
                "type": "string",
                "description": "Additional context or instructions for the sub-agent (e.g. 'use OCR library v2', 'prefer the smallest tool'). Keep short — this becomes part of the child's system prompt. Same caveat as `task`: do not paste large file contents here."
            }),
        );
        props.insert(
            "attachments".into(),
            json!({
                "type": "array",
                "description": "Image / file attachments the child should see. PASS PATHS (e.g. 'D:\\\\imgs\\\\a.jpg') — NEVER base64-encode inline. Each entry is read from disk and injected as a multimodal ImageUrl into the child's first user message so the child LLM can actually see the pixels.\n\n\
                 Today only `kind:'image'` is forwarded (PNG/JPEG/GIF/WEBP/BMP). Other kinds are ignored. For non-image data (PDFs, documents, code, logs), do NOT use this field — put the path in `task` or `context` and let the sub-agent read it via its own tools (read_file in chunks, execute_command pdftotext, etc.).\n\n\
                 L1 CAPACITY: pass ≤ 10 attachments per delegate_task call. For more items, split into multiple delegate_task calls (each ≤ 10) — a single call with >10 attachments will overflow both parent and child context. Future: a `delegate_batch` tool will handle >10 with Rust-side executor.",
                "items": json!({
                    "type": "object",
                    "properties": {
                        "path": json!({
                            "type": "string",
                            "description": "Local filesystem path to the image (PNG/JPEG/GIF/WEBP/BMP)."
                        }),
                        "detail": json!({
                            "type": "string",
                            "enum": ["preview", "auto", "full"],
                            "description": "Vision detail level. Default 'auto' (~1568px). 'preview' is fastest, 'full' is highest resolution."
                        }),
                        "kind": json!({
                            "type": "string",
                            "enum": ["image"],
                            "description": "Attachment type. Currently only 'image' is supported; non-image attachments are ignored."
                        })
                    },
                    "required": ["path"]
                })
            }),
        );
        props.insert(
            "output_schema".into(),
            json!({
                "type": "string",
                "description": "Expected output format for the child's final answer. Either a JSON Schema (e.g. '{\"plate\":\"string\",\"model\":\"string\"}') or a natural-language shape description (e.g. 'one line per image: <path> -> <result>'). Injected into the child's system prompt so it conforms. STRONGLY RECOMMENDED whenever you need to parse/aggregate the child's reply — without this, the child may reply in markdown prose that is hard to merge across many sub-agents."
            }),
        );
        props.insert(
            "toolset".into(),
            json!({
                "type": "string",
                "description": "Toolset name to assign to the sub-agent (e.g. 'web', 'terminal'). Avoid passing names of tools that are blocked for sub-agents (delegate_task, clarify, memory, send_message, cronjob) — you'll get an early error."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Model to use for the sub-agent (default: same as parent). Use a cheaper model for simple tasks (OCR, classification) to save cost."
            }),
        );
        props.insert(
            "role".into(),
            json!({
                "type": "string",
                "enum": ["general", "explore", "plan"],
                "description": "Sub-agent role. 'explore' = read-only (good for research/lookups). 'plan' = read + planning tools. 'general' = full toolset (default). Overrides the role declared by an `agent` profile."
            }),
        );
        props.insert(
            "agent".into(),
            json!({
                "type": "string",
                "description": "Named sub-agent profile from <HERMIX_HOME>/agents/<name>.md (built-ins: 'explore', 'general-purpose'). A profile carries a role (its tool whitelist), a default turn budget, a tool hint list and a system-prompt body. Use `explore` for read-only research so the child physically cannot write."
            }),
        );
        props.insert(
            "max_iterations".into(),
            json!({
                "type": "integer",
                "description": "Override the child's max_turns budget. Default is derived from parent remaining turns (clamped to [2, 12]). Set explicitly when you know the task needs more turns (e.g. multi-image analysis)."
            }),
        );
        props.insert(
            "parent_task_id".into(),
            json!({
                "type": "string",
                "description": "If this delegation is part of a parent task tree, pass the parent's task id so results can be attributed."
            }),
        );
        props.insert(
            "background".into(),
            json!({
                "type": "boolean",
                "description": "Run the sub-agent detached. When true, returns {\"status\":\"spawned\",\"sub_agent_id\":…} immediately instead of blocking; poll `get_subagent_result` with that id to collect the result. Use it when the child's work is independent of your next step and you want to keep working (e.g. long research while continuing the main edit). The child still obeys depth/role/turn caps and inherits the parent budget."
            }),
        );
        props.insert(
            "child_depth".into(),
            json!({
                "type": "integer",
                "description": "Current child-agent depth (injected by orchestration layer, do not set manually)"
            }),
        );
        props.insert(
            "max_depth".into(),
            json!({
                "type": "integer",
                "description": "Maximum delegation depth allowed for this request (injected by orchestration layer, do not set manually)"
            }),
        );
        props.insert(
            "parent_budget_remaining_usd".into(),
            json!({
                "type": "number",
                "description": "Remaining parent budget in USD to propagate to child orchestration (injected, do not set manually)"
            }),
        );

        tool_schema(
            "delegate_task",
            "Delegate a task to a sub-agent with an isolated context. By default SYNCHRONOUS: blocks until the sub-agent finishes and returns its result as a normal tool result — you do NOT need to poll. With `background: true` it returns {\"status\":\"spawned\",\"sub_agent_id\":…} immediately and you collect the result later via `get_subagent_result`. The sub-agent's text conclusion is capped at ~8,000 chars.\n\n\
             # When to use\n\
             - The task is genuinely independent of your current focus (no shared state needed mid-task).\n\
             - You want a separate context (e.g. image analysis where you don't want the pixels in YOUR context).\n\
             - Multiple sub-agents can run CONCURRENTLY in a single round — list multiple delegate_task calls in one assistant turn and they execute in parallel.\n\n\
             # Task discipline\n\
             - ONE focused objective per delegation. A multi-step plan is a series of delegations, not one mega-task.\n\
             - If a delegation will plausibly take >3 minutes, SPLIT it into smaller independent pieces or pass `background: true` and continue other work — never leave the main loop blocked on one long child.\n\
             - FAIL FAST: if the sub-agent hits a blocker, it must return and report instead of retrying endlessly — a rejected/failed child costs far less than a wedged one.\n\n\
             # L1 capacity bounds (hard limits of this tool)\n\
             - ≤ 10 attachments per call (more overflows context).\n\
             - ≤ 4 sub-agents running concurrently (hard server-side cap; excess calls return \"rejected\" — wait for one to finish, then re-dispatch).\n\
             - LLM does NOT have a persistent task queue — if you start a fan-out, you must wait for results in the same round. Don't say 'process the remaining 90' and expect the next round to remember.\n\n\
             # For larger batches (10–500 items)\n\
             Split into multiple delegate_task calls across rounds yourself, OR tell the user this is L2 territory (Rust-side executor — not yet implemented). Don't fake it by sending >10 attachments in one call.",
            JsonSchema::object(props, vec!["task".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDelegationBackend;
    #[async_trait]
    impl DelegationBackend for MockDelegationBackend {
        async fn delegate(
            &self,
            task: &str,
            _context: Option<&str>,
            _toolset: Option<&str>,
            _model: Option<&str>,
            _child_depth: Option<u32>,
            _max_depth: Option<u32>,
            _parent_budget_remaining_usd: Option<f64>,
        ) -> Result<String, ToolError> {
            Ok(format!("Delegated task: {}", task))
        }
    }

    #[tokio::test]
    async fn test_delegate_task_schema() {
        let handler = DelegateTaskHandler::new(Arc::new(MockDelegationBackend));
        assert_eq!(handler.schema().name, "delegate_task");
    }

    #[tokio::test]
    async fn test_delegate_task_execute() {
        let handler = DelegateTaskHandler::new(Arc::new(MockDelegationBackend));
        let result = handler
            .execute(json!({"task": "Research AI trends"}))
            .await
            .unwrap();
        assert!(result.contains("Research AI trends"));
    }
}
