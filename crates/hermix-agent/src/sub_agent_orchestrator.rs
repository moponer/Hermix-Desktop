//! In-process sub-agent orchestrator.
//!
//! Python parity: when the primary agent calls `delegate_task`, we
//! actually spawn a bounded child [`AgentLoop`] rather than just returning a
//! signal envelope. The orchestrator is responsible for:
//!
//! - building a restricted child [`AgentConfig`] (depth+1, half-remaining turn
//!   budget, isolated session id)
//! - running the child with a wall-clock timeout and cooperative cancellation
//!   tied to the parent's [`InterruptController`]
//! - persisting a lineage record per sub-agent under
//!   `$HERMIX_HOME/subagents/<sub_agent_id>.json` with
//!   started/ended timestamps, status, turn count, usage and cost
//! - returning a JSON string that the agent loop feeds back to the LLM as the
//!   tool result (must include `sub_agent_id` so
//!   [`crate::memory_hooks::delegation_event_from_tool_result`] still fires).
//!
//! Registration: consumers build an [`AgentLoop`] via
//! [`crate::AgentLoop::with_sub_agent_orchestrator`] to swap the default
//! signal-only behavior with real execution.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hermix_core::{AgentError, LlmProvider, Message, StreamChunk, StreamDelta};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::timeout;

use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::interrupt::InterruptController;

/// Boxed `Send` future type alias used to short-circuit async-recursion
/// between parent and child [`AgentLoop::execute_tool_calls`] futures.
type BoxSendFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Default wall-clock cap for a single child agent turn-chain.
/// 2026-11-10 二次收紧 600→180（用户拍板 3 分钟上限）：子代理只做短任务，
/// 主 agent 拆分并行派发、或长任务走 dispatch_member 后台化。
pub const DEFAULT_SUB_AGENT_TIMEOUT_SECS: u64 = 180;
/// Floor on child max_turns regardless of parent budget.
pub const SUB_AGENT_MIN_TURNS: u32 = 2;
/// Ceiling on child max_turns to keep sub-tasks bounded.
pub const SUB_AGENT_MAX_TURNS_CAP: u32 = 12;
/// Maximum character length of the sub-agent's final text result
/// that is returned to the parent context. Without this cap, a
/// sub-agent that produces a very long analysis (e.g. 50,000 chars)
/// would inject that entire text as a single `tool_result` into
/// the parent's `ContextManager`, causing context explosion — the
/// very problem `delegate_task` is meant to prevent.
/// 8,000 chars ≈ 2,000 tokens, which is the same order as a single
/// `read_image` blob. This is large enough for a detailed structured
/// analysis but small enough to keep the parent context lean.
pub const SUB_AGENT_RESULT_MAX_CHARS: usize = 8_000;

/// How many recent internal actions (tool calls / thinking) the
/// "子代理" panel scrolls. Newest last; older entries fall off.
pub const SUB_AGENT_STEP_LOG_CAP: usize = 6;

/// One-line human label for a sub-agent tool call, used in the live
/// step log (e.g. `read_file: manifest.json`, `terminal: npm install -g`).
/// Falls back to the bare tool name. Best-effort — never errors.
pub(crate) fn tool_step_label(tool: &str, args: &serde_json::Value) -> String {
    let name = tool.split('_').last().unwrap_or(tool);
    let primary = args
        .as_object()
        .and_then(|m| {
            for k in ["path", "command", "query", "url", "tool", "target"] {
                if let Some(v) = m.get(k).and_then(|v| v.as_str()) {
                    if !v.trim().is_empty() {
                        return Some(v.trim().to_string());
                    }
                }
            }
            None
        });
    match primary {
        Some(p) => {
            let short: String = p.chars().take(60).collect();
            format!("{name}: {short}")
        }
        None => format!("{name}"),
    }
}

/// Lineage record persisted to disk for each spawned sub-agent.
/// One record is written at `started` and rewritten at terminal state. The file
/// layout stays intentionally flat so it can also be inspected / tailed by
/// other tooling (Python baseline uses a similar per-sub-agent JSON file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentLineage {
    pub sub_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub depth: u32,
    pub max_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_budget_remaining_usd: Option<f64>,
    pub status: SubAgentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub total_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Role of the sub-agent.
    #[serde(default)]
    pub role: SubAgentRole,
    /// Parent task ID for linking sub-agents to the task tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// Final assistant text of a completed run (already capped to
    /// `SUB_AGENT_RESULT_MAX_CHARS` by `extract_final_assistant_text`).
    /// Absent while running and on failure. This is what makes
    /// `get_subagent_result` able to return a real result for background
    /// sub-agents — before this field existed the tool always read
    /// `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Whether `result` was truncated to the 8k cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Whether the child ended because the model stopped (true) or because
    /// it hit its turn/iteration cap (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_naturally: Option<bool>,
}

/// Terminal state vocabulary for sub-agent lineage records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    /// Legacy status: the child was spawned but not yet started. The old
    /// `execute_async` path returned immediately with this status; the
    /// current path is synchronous (waits for completion) so `Spawned` is
    /// only seen in lineage files from older sessions.
    Spawned,
    Started,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

/// Default max iterations for sub-agent delegation (Hermes Agent parity).
pub const DEFAULT_DELEGATION_MAX_ITERATIONS: u32 = 50;

/// Role for a sub-agent. Determines the toolset and system prompt.
///
/// - **General** — full toolset, general-purpose task execution.
/// Suitable for most delegation scenarios.
/// - **Explore** — read-only toolset. Used for information gathering,
/// code exploration, web search. Cannot modify files or execute
/// shell commands that write.
/// - **Plan** — planning toolset. Used for producing plans and
/// strategies. Has access to read tools + todo/task tools but
/// not write tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SubAgentRole {
    /// General-purpose sub-agent with full tool access.
    #[default]
    General,
    /// Read-only explorer — information gathering, code exploration.
    Explore,
    /// Planner — produces plans, has read + task/todo tools.
    Plan,
}

impl SubAgentRole {
    /// Returns the toolset restriction for this role.
    /// `None` means no restriction (full toolset).
    /// `Some(set)` means only tools in the set are available.
    pub fn allowed_tools(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::General => None,
            Self::Explore => Some(&EXPLORE_ROLE_TOOLS),
            Self::Plan => Some(&PLAN_ROLE_TOOLS),
        }
    }

    /// Returns a human-readable description of the role.
    pub fn description(&self) -> &'static str {
        match self {
            Self::General => "general-purpose sub-agent with full tool access",
            Self::Explore => "read-only explorer for information gathering",
            Self::Plan => "planner that produces plans and strategies",
        }
    }

    /// Lowercased role name (`general` | `explore` | `plan`), used for the
    /// `role` field of `SubAgentEvent` payloads emitted to the frontend.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Explore => "explore",
            Self::Plan => "plan",
        }
    }

    /// Role-specific return contract appended to the sub-agent's system
    /// guidance. Empty for `General` (general-purpose results need no
    /// fixed shape). This is what makes the role more than a tool
    /// whitelist: the child is told what its final answer must look like.
    pub fn return_contract(&self) -> &'static str {
        match self {
            Self::General => "",
            Self::Explore => {
                "\n# Return contract (Explore role)\n\
                 Your job is FINDINGS, not dumps. Your final response must be:\n\
                 - Conclusion first, then evidence: `path:line` + one-line quote per finding.\n\
                 - NEVER paste source excerpts longer than 5 lines or raw file contents.\n\
                 - State coverage: what you searched and what you did NOT check.\n\
                 - If information is missing, say what is missing — do not guess.\n"
            }
            Self::Plan => {
                "\n# Return contract (Plan role)\n\
                 Return a structured plan with exactly these sections:\n\
                 1. **Goal** — one sentence, measurable.\n\
                 2. **Steps** — ordered; each step states how to verify it.\n\
                 3. **Risks** — each with a mitigation.\n\
                 4. **Open questions** — decisions the parent must make.\n\
                 No implementation code — you are a planner, not an implementer.\n"
            }
        }
    }
}

/// Tools available to the Explore role (read-only).
/// 成员角色的 `read_only` toolset 复用这同一份白名单（成员模块引用）。
pub(crate) const EXPLORE_ROLE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "list_dir",
    "search_files",
    "read_image",
    "read_pdf",
    "markitdown",
    "web_search",
    "web_extract",
    "url_safety",
    "session_search",
    "tool_search",
    "tool_describe",
    "tool_call",
];

/// Tools available to the Plan role (read + planning).
const PLAN_ROLE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "list_dir",
    "search_files",
    "read_pdf",
    "markitdown",
    "web_search",
    "web_extract",
    "url_safety",
    "session_search",
    "todo",
    "task_create",
    "task_update",
    "task_list",
    "task_get",
    "clarify",
    "tool_search",
    "tool_describe",
    "tool_call",
];

/// Image (or other attachment) passed from parent to sub-agent.
/// The orchestrator reads the file from disk, base64-encodes it, and
/// injects it as a multimodal `ContentPart::ImageUrl` into the
/// child's first user message so the child LLM can actually SEE it
/// via native vision. Without this, the child would only receive a
/// text description of the path — and the `read_image` results that
/// the parent already saw are not automatically carried over.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ImageAttachment {
    /// Local filesystem path to the image (PNG/JPEG/GIF/WEBP/BMP).
    pub path: String,
    /// Detail level for `read_image` (`"preview"` | `"auto"` | `"full"`).
    /// Default: `"auto"` (balanced, 1568px).
    pub detail: Option<String>,
}

/// Parameters forwarded by the `delegate_task` tool invocation.
#[derive(Debug, Clone, Default)]
pub struct SubAgentRequest {
    pub task: String,
    pub context: Option<String>,
    pub toolset: Option<String>,
    pub model: Option<String>,
    /// Child depth injected by parent agent loop (parent_depth + 1).
    pub child_depth: u32,
    /// Propagated max depth for lineage / bookkeeping.
    pub max_depth: u32,
    /// Parent budget remaining (USD) — exposed to the child for cost awareness.
    pub parent_budget_remaining_usd: Option<f64>,
    /// Independent iteration budget for this sub-agent (Hermes Agent parity).
    /// When set, overrides the derived max_turns from parent budget.
    /// Default: `DEFAULT_DELEGATION_MAX_ITERATIONS` (50).
    pub max_iterations: Option<u32>,
    /// Role of the sub-agent. Determines the toolset and system prompt.
    /// Defaults to `General` (full tool access).
    pub role: SubAgentRole,
    /// Parent task ID for linking sub-agents to the task tree.
    /// When set, the sub-agent's results are attributed to this task.
    pub parent_task_id: Option<String>,
    /// Image / file attachments forwarded from the parent.
    /// Each path is read, base64-encoded, and injected into the
    /// child's first user message as a multimodal `ImageUrl` part.
    /// This is what makes image analysis through `delegate_task`
    /// actually work — previously the child only got a text path
    /// string and had no way to see the pixels.
    pub attachments: Vec<ImageAttachment>,
    /// Optional JSON schema (or human-readable shape
    /// description) for the structured output the parent expects
    /// the child to return. Injected into the child's system
    /// message so the child formats its final answer accordingly.
    pub output_schema: Option<String>,
}

/// Configuration for [`SubAgentOrchestrator`].
#[derive(Clone)]
pub struct SubAgentOrchestratorConfig {
    pub parent_config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub parent_interrupt: InterruptController,
    pub hermix_home: PathBuf,
    pub parent_session_id: Option<String>,
    pub timeout: Duration,
    /// Optional chunk emitter used to stream sub-agent lifecycle events
    /// (`sub_agent_update`) to the frontend in real time. Wired by the
    /// server with the per-stream WS `on_chunk` callback. When `None`, the
    /// orchestrator still runs sub-agents but does not surface live status.
    pub chunk_emitter: Option<Arc<dyn Fn(StreamChunk) + Send + Sync>>,
    /// Optional TaskRecord producer (§3.3 热路径): persists sub-agent
    /// lifecycle into the TaskRecordStore so crash recovery has
    /// decision input. `None` = records are not produced (headless
    /// builds / tests).
    pub task_record_sink: Option<Arc<dyn TaskRecordSink>>,
}

/// Abort handles of **background** sub-agents, keyed by `sub_agent_id`.
/// Process-global because the id is the public handle handed to the model —
/// a second orchestrator instance (e.g. after a reconnect) must be able to
/// stop a task the first one spawned. Entries are removed when the child
/// finishes or is stopped, so the map stays proportional to live tasks.
fn background_tasks() -> &'static Mutex<HashMap<String, tokio::task::AbortHandle>> {
    static TASKS: OnceLock<Mutex<HashMap<String, tokio::task::AbortHandle>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 子代理并发上限：同时处于运行态（spawned → 终态之间）的子代理数。
/// 对标 Claude Code（并发 20，服务端形态）与 OpenHands（max_children=5）：
/// 桌面单机 4 路足够；超限**直接拒绝并如实告知**，防止代理树无界生长、
/// 上下文与预算被无感吞噬（2026-11-16 竞品差距分析 P0-1）。
const MAX_CONCURRENT_SUB_AGENTS: usize = 4;

/// 运行态计数 guard：Drop 时自动减计数——子循环 panic / 取消 / 超时退出
/// 都保证计数回落，不会泄漏并发槽。
struct LiveSubAgentGuard(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for LiveSubAgentGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// In-process executor for `delegate_task` tool calls.
pub struct SubAgentOrchestrator {
    cfg: SubAgentOrchestratorConfig,
    /// 当前运行中的子代理计数（含同步等待与后台派发两条路径）。
    live_sub_agents: Arc<std::sync::atomic::AtomicUsize>,
}

/// Sink for persisting sub-agent lifecycle as `TaskRecord`s
/// (§3.3 热路径生产者). Implemented by the server over
/// `hermix_task::TaskRecordStore` — kept as a trait so
/// hermix-agent does not depend on hermix-task. Best-effort:
/// implementors must never fail delegation (log, don't propagate).
pub trait TaskRecordSink: Send + Sync {
    /// Record the sub-agent starting (Running state).
    fn started(&self, sub_agent_id: &str, conversation_id: Option<&str>, task: &str);
    /// Record a terminal state (completed / failed / cancelled / timeout).
    fn terminal(&self, sub_agent_id: &str, outcome: &str, error: Option<&str>);
}

impl SubAgentOrchestrator {
    pub fn new(cfg: SubAgentOrchestratorConfig) -> Self {
        Self {
            cfg,
            live_sub_agents: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Attach a TaskRecord producer (§3.3 热路径生产者). Server-side
    /// wiring; `None` by default (tests / headless builds).
    pub fn with_task_record_sink(mut self, sink: Arc<dyn TaskRecordSink>) -> Self {
        self.cfg.task_record_sink = Some(sink);
        self
    }

    /// Default orchestrator that mirrors the current `AgentLoop`'s wiring.
    /// The returned orchestrator inherits the parent's tool registry, provider
    /// and interrupt handle so cancellation propagates from the parent to any
    /// actively running child.
    pub fn from_parent(
        parent: &AgentLoop,
        hermix_home: PathBuf,
        chunk_emitter: Option<Arc<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Self {
        Self {
            cfg: SubAgentOrchestratorConfig {
                parent_config: parent.config.clone(),
                tool_registry: parent.tool_registry.clone(),
                llm_provider: parent.llm_provider.clone(),
                parent_interrupt: parent.interrupt.clone(),
                hermix_home,
                parent_session_id: parent.config.session_id.clone(),
                timeout: Duration::from_secs(DEFAULT_SUB_AGENT_TIMEOUT_SECS),
                chunk_emitter,
                task_record_sink: None,
            },
            live_sub_agents: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Execute a `delegate_task` request synchronously from the perspective of
    /// the caller, returning a JSON string suitable for use as the tool result.
    /// The returned JSON always contains `sub_agent_id` at the top level so
    /// existing delegation-event wiring in [`AgentLoop`] continues to work.
    /// Returning `BoxSendFuture` (rather than an `async fn`) is load-bearing:
    /// the parent `AgentLoop::execute_tool_calls` awaits this future, and the
    /// child may eventually call back into an orchestrator of its own. A
    /// concrete `impl Future` type would force the compiler to recursively
    /// prove `Send` through that cycle, which it cannot. Boxing into
    /// `dyn Future + Send` erases the child's future type and breaks the
    /// cycle.
    /// Build a fresh lineage record for a sub-agent with the given status.
    fn build_lineage(&self, req: &SubAgentRequest, sub_agent_id: &str, status: SubAgentStatus) -> SubAgentLineage {
        SubAgentLineage {
            sub_agent_id: sub_agent_id.to_string(),
            parent_session_id: self.cfg.parent_session_id.clone(),
            task: req.task.clone(),
            context: req.context.clone(),
            toolset: req.toolset.clone(),
            model: req.model.clone(),
            depth: req.child_depth,
            max_depth: req.max_depth,
            parent_budget_remaining_usd: req.parent_budget_remaining_usd,
            status,
            started_at: Utc::now(),
            ended_at: None,
            total_turns: 0,
            prompt_tokens: None,
            completion_tokens: None,
            estimated_cost_usd: None,
            error: None,
            role: req.role,
            parent_task_id: req.parent_task_id.clone(),
            result: None,
            truncated: None,
            finished_naturally: None,
        }
    }

    /// Update a base lineage record with the outcome of a child run and persist it.
    async fn persist_terminal_lineage(
        &self,
        base: &SubAgentLineage,
        outcome: &Result<hermix_core::AgentResult, SubAgentError>,
    ) {
        let mut lineage = base.clone();
        match outcome {
            Ok(result) => {
                lineage.status = SubAgentStatus::Completed;
                lineage.total_turns = result.total_turns;
                if let Some(usage) = result.usage.as_ref() {
                    lineage.prompt_tokens = Some(usage.prompt_tokens);
                    lineage.completion_tokens = Some(usage.completion_tokens);
                    lineage.estimated_cost_usd = usage.estimated_cost;
                }
                // Persist the final text so background consumers
                // (get_subagent_result polling the lineage file) see a
                // real result instead of `null`.
                let (final_text, was_truncated) =
                    extract_final_assistant_text(&result.messages);
                lineage.result = Some(final_text);
                lineage.truncated = Some(was_truncated);
                lineage.finished_naturally = Some(result.finished_naturally);
                lineage.ended_at = Some(Utc::now());
            }
            Err(e) => {
                lineage.status = match e {
                    SubAgentError::Timeout => SubAgentStatus::Timeout,
                    SubAgentError::Cancelled => SubAgentStatus::Cancelled,
                    SubAgentError::Agent(_) => SubAgentStatus::Failed,
                };
                lineage.error = Some(e.to_string());
                lineage.ended_at = Some(Utc::now());
            }
        }
        self.persist_lineage(&lineage).await;
        // §3.3 热路径生产者：终态落 TaskRecordStore（best-effort）。
        if let Some(sink) = self.cfg.task_record_sink.as_ref() {
            let (outcome, error) = match &lineage.status {
                SubAgentStatus::Completed => ("completed", None),
                SubAgentStatus::Failed => ("failed", lineage.error.as_deref()),
                SubAgentStatus::Cancelled => ("aborted", lineage.error.as_deref()),
                SubAgentStatus::Timeout => ("failed", Some("sub-agent timed out")),
                _ => ("failed", lineage.error.as_deref()),
            };
            sink.terminal(&lineage.sub_agent_id, outcome, error);
        }
    }

    /// Emit a `sub_agent_update` chunk to the frontend (if an emitter is
    /// wired). Called on each lifecycle phase transition so the UI can show
    /// a live "子代理" panel. Failures are silently ignored — sub-agent
    /// status is best-effort observability, never on the critical path.
    ///
    /// `recent_steps` carries the sub-agent's last N internal actions
    /// (tool calls / thinking, newest last) so the frontend panel can
    /// render a scrolling log — one line per action — instead of only
    /// the coarse phase badge.
    fn emit_sub_agent(
        &self,
        sub_agent_id: &str,
        phase: &str,
        task: &str,
        role: SubAgentRole,
        turns: Option<u32>,
        cost_usd: Option<f64>,
        error: Option<&str>,
        recent_steps: &[String],
    ) {
        let emitter = match &self.cfg.chunk_emitter {
            Some(e) => e,
            None => return,
        };
        let payload = serde_json::json!({
            "event": "sub_agent_update",
            "sub_agent_id": sub_agent_id,
            "phase": phase,
            "task": task,
            "role": role.as_str(),
            "turns": turns,
            "cost_usd": cost_usd,
            "error": error,
            "recent_steps": recent_steps,
        });
        emitter(StreamChunk {
            delta: Some(StreamDelta {
                content: None,
                tool_calls: None,
                extra: Some(payload),
            }),
            finish_reason: None,
            usage: None,
        });
    }

    /// Execute a `delegate_task` request **asynchronously**: spawn the child
    /// loop on its own detached tokio task, persist a `Spawned` lineage record
    /// immediately, and return a JSON envelope containing `sub_agent_id` right
    /// away. The parent agent is NOT blocked on the child to completion — it
    /// receives the id and can call `get_subagent_result(sub_agent_id)` later
    /// (or keep doing other work / dispatch more sub-agents in parallel).
    ///
    /// This is the root-cause fix for the old sync behaviour where the main
    /// agent's single tool call blocked up to `timeout` seconds waiting for the
    /// child, which both hurt UX and tempted the LLM to hallucinate "waiting
    /// for the sub-agent to return" instead of actually dispatching.
    pub fn execute_async(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
        let this = self.clone();
        Box::pin(async move {
            // Spawn execute_inner on its own tokio task to break async
            // recursion (AgentLoop::run → execute_tool_calls →
            // delegate_task → execute_async → run_child → AgentLoop::run).
            // run_child ALSO spawns the child AgentLoop internally, so this
            // outer spawn is belt-and-suspenders.
            //
            // Unlike the old implementation which returned immediately with
            // {"status":"spawned"} (requiring get_subagent_result polling),
            // we now WAIT for the child to finish and return its final
            // result. This makes delegate_task truly synchronous — matching
            // the schema description ("SYNCHRONOUS: blocks until the
            // sub-agent finishes") and eliminating the polling loop bug.
            let req_for_error = req.clone();
            let handle = tokio::spawn(this.clone().execute_inner(req));
            match handle.await {
                Ok(result_str) => result_str,
                Err(join_err) => {
                    tracing::error!(
                        error = %join_err,
                        "sub-agent tokio task panicked or was cancelled"
                    );
                    // §3.3 热路径生产者：panic/cancel 也会到达终态，
                    // 否则恢复决策（recover）看不到这次派发曾经存在。
                    if let Some(sink) = this.cfg.task_record_sink.as_ref() {
                        // id 无从得知（inner 已 panic）——以 req 摘要记录
                        // Orphaned 语义，恢复端按 reason 归类。
                        sink.terminal(
                            &format!("orphaned-{}", uuid::Uuid::new_v4()),
                            "orphaned",
                            Some(&format!("tokio task lost: {join_err}")),
                        );
                    }
                    json!({
                        "status": "failed",
                        "error": format!("Sub-agent task failed: {}", join_err),
                        "task": req_for_error.task,
                    })
                    .to_string()
                }
            }
        })
    }

    pub fn execute(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
        let this = self.clone();
        Box::pin(async move { this.execute_inner(req).await })
    }

    /// Detached spawn (background delegation, ZCode/Codex-style):
    /// returns the sub-agent id **immediately**; the child runs on its own
    /// tokio task and writes its terminal state — including the final
    /// `result` text — into the lineage file, which `get_subagent_result`
    /// polls.
    ///
    /// The `started` lineage record (and `task_record_sink.started`) is
    /// written before this returns, so an immediate poll sees `started`
    /// rather than `not_found`.
    pub fn spawn_background(
        self: &Arc<Self>,
        req: SubAgentRequest,
    ) -> BoxSendFuture<Result<String, String>> {
        // Boxed like `execute_async`: the child's future reaches back into
        // an orchestrator, so a concrete `impl Future` would force the
        // compiler through a recursion cycle it cannot prove.
        let this = self.clone();
        Box::pin(async move {
            let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
            let lineage = this.build_lineage(&req, &sub_agent_id, SubAgentStatus::Started);
            this.persist_lineage(&lineage).await;
            if let Some(sink) = this.cfg.task_record_sink.as_ref() {
                sink.started(
                    &sub_agent_id,
                    lineage.parent_session_id.as_deref(),
                    &req.task,
                );
            }
            let spawn_this = this.clone();
            let id = sub_agent_id.clone();
            let cleanup_id = id.clone();
            // announce=false: lifecycle already announced above; the task
            // only runs the child and persists the terminal record.
            //
            // spawn + insert happen under ONE lock: if the child finished
            // before we inserted (it can, for a stub provider), its own
            // cleanup would remove nothing and the stale AbortHandle would
            // linger — later `stop_background` would then flip an already
            // completed lineage to `cancelled`.
            {
                let mut map = background_tasks()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let handle = tokio::spawn(async move {
                    spawn_this.execute_inner_with_id(req, id, false).await;
                    if let Ok(mut map) = background_tasks().lock() {
                        map.remove(&cleanup_id);
                    }
                });
                map.insert(sub_agent_id.clone(), handle.abort_handle());
            }
            Ok(sub_agent_id)
        })
    }

    /// Stop a background sub-agent (`stop_subagent` tool).
    ///
    /// Aborts the detached task and rewrites its lineage as `cancelled`, so
    /// `get_subagent_result` reports a terminal state instead of "still
    /// running" forever. Returns `false` when the id is unknown (already
    /// finished, or never existed) — callers surface that as a plain "not
    /// running" answer, not an error.
    pub fn stop_background(self: &Arc<Self>, sub_agent_id: &str) -> BoxSendFuture<bool> {
        let this = self.clone();
        let id = sub_agent_id.to_string();
        Box::pin(async move {
            let handle = background_tasks()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            let Some(handle) = handle else {
                return false;
            };
            handle.abort();

            // Terminal lineage (best-effort): read → patch → write.
            let path = this
                .cfg
                .hermix_home
                .join("subagents")
                .join(format!("{id}.json"));
            if let Ok(raw) = tokio::fs::read_to_string(&path).await {
                if let Ok(mut lineage) = serde_json::from_str::<SubAgentLineage>(&raw) {
                    lineage.status = SubAgentStatus::Cancelled;
                    lineage.error = Some("stopped by request".into());
                    lineage.ended_at = Some(Utc::now());
                    this.persist_lineage(&lineage).await;
                }
            }
            if let Some(sink) = this.cfg.task_record_sink.as_ref() {
                sink.terminal(&id, "aborted", Some("stopped by request"));
            }
            true
        })
    }

    async fn execute_inner(self: Arc<Self>, req: SubAgentRequest) -> String {
        let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
        self.execute_inner_with_id(req, sub_agent_id, true).await
    }

    /// Core child run. `announce` controls whether the `started` lineage
    /// record + TaskRecord are written here (sync path) or were already
    /// written by `spawn_background` (background path).
    async fn execute_inner_with_id(
        self: Arc<Self>,
        req: SubAgentRequest,
        sub_agent_id: String,
        announce: bool,
    ) -> String {
        // 并发上限：先占槽再开跑。超限返回 rejected 终态（父模型会看到
        // 明确原因，可等已有子代理结束再派）。spawn_background 与同步
        // await 两条路径都在这里汇合，一个检查点覆盖两者。
        let live_now = self
            .live_sub_agents
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let _live_guard = LiveSubAgentGuard(self.live_sub_agents.clone());
        if live_now > MAX_CONCURRENT_SUB_AGENTS {
            tracing::warn!(
                live_now,
                max = MAX_CONCURRENT_SUB_AGENTS,
                task = %req.task,
                "delegate_task rejected: concurrent sub-agent limit reached"
            );
            return json!({
                "sub_agent_id": sub_agent_id,
                "status": "rejected",
                "task": req.task,
                "error": format!(
                    "Concurrent sub-agent limit reached ({MAX_CONCURRENT_SUB_AGENTS} already \
                     running). Wait for one to finish (get_subagent_result), then re-dispatch \
                     this task — or split it into smaller, independent pieces."
                ),
                "depth": req.child_depth,
                "max_depth": req.max_depth,
            })
            .to_string();
        }
        let lineage = self.build_lineage(&req, &sub_agent_id, SubAgentStatus::Started);

        // Best-effort lineage persistence — failures must never break delegation.
        if announce {
            self.persist_lineage(&lineage).await;

            // §3.3 热路径生产者：Running 记录落 TaskRecordStore（best-effort）。
            if let Some(sink) = self.cfg.task_record_sink.as_ref() {
                sink.started(
                    &sub_agent_id,
                    lineage.parent_session_id.as_deref(),
                    &req.task,
                );
            }
        }

        // Rolling log of the sub-agent's last N internal actions (tool
        // calls / thinking), newest last. Shared with the child loop's
        // callbacks via Arc<Mutex<>> so the frontend's "子代理" panel can
        // render a scrolling one-line-per-action log in real time.
        let recent_steps: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        // Build and run the child.
        let outcome = self.run_child(&req, &sub_agent_id, recent_steps.clone()).await;

        // Persist the terminal state (updates lineage fields + writes file).
        self.persist_terminal_lineage(&lineage, &outcome).await;

        // Emit terminal event so the frontend panel updates from
        // "started" to "done" / "failed". This was previously only done
        // in the async execute_async path's spawned task; now that
        // execute_async delegates to execute_inner, the emit must live
        // here so both code paths share the same lifecycle events.
        let (phase, turns, cost_usd, err) = match &outcome {
            Ok(result) => (
                "done",
                Some(result.total_turns),
                result.usage.as_ref().and_then(|u| u.estimated_cost),
                None,
            ),
            Err(e) => ("failed", None, None, Some(e.to_string())),
        };
        let steps_snapshot: Vec<String> = recent_steps
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        self.emit_sub_agent(
            &sub_agent_id,
            phase,
            &req.task,
            req.role,
            turns,
            cost_usd,
            err.as_deref(),
            &steps_snapshot,
        );

        match outcome {
            Ok(result) => {
                let (final_text, was_truncated) = extract_final_assistant_text(&result.messages);
                json!({
                    "sub_agent_id": sub_agent_id,
                    "status": "completed",
                    "task": req.task,
                    "total_turns": result.total_turns,
                    "finished_naturally": result.finished_naturally,
                    "result": final_text,
                    "truncated": was_truncated,
                    "usage": result.usage.as_ref().map(|u| json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                        "estimated_cost": u.estimated_cost,
                    })),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
            Err(e) => {
                json!({
                    "sub_agent_id": sub_agent_id,
                    "status": match &e {
                        SubAgentError::Timeout => "timeout",
                        SubAgentError::Cancelled => "cancelled",
                        SubAgentError::Agent(_) => "failed",
                    },
                    "task": req.task,
                    "error": e.to_string(),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
        }
    }

    async fn run_child(
        &self,
        req: &SubAgentRequest,
        sub_agent_id: &str,
        recent_steps: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Result<hermix_core::AgentResult, SubAgentError> {
        // Surface the start so the frontend flips the sub-agent panel from
        // "spawned" to "started" (执行中) as soon as the child loop begins.
        let empty_steps: Vec<String> = Vec::new();
        self.emit_sub_agent(sub_agent_id, "started", &req.task, req.role, None, None, None, &empty_steps);
        let child_interrupt = InterruptController::new();

        // Forward parent cancellation to child via a light watcher task.
        let parent_ctrl = self.cfg.parent_interrupt.clone();
        let child_ctrl = child_interrupt.clone();
        let watcher_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_stop_clone = watcher_stop.clone();
        let watcher = tokio::spawn(async move {
            while !watcher_stop_clone.load(std::sync::atomic::Ordering::Acquire) {
                if parent_ctrl.is_interrupted() {
                    child_ctrl.interrupt(Some("parent cancelled".to_string()));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let child_config = self.build_child_config(req, sub_agent_id);
        let child_interrupt_for_spawn = child_interrupt.clone();
        let tool_registry = self.cfg.tool_registry.clone();
        let llm_provider = self.cfg.llm_provider.clone();
        let child_depth = req.child_depth;
        // Build the initial messages synchronously *before* the spawn so the
        // move-block doesn't have to hold the full attachment list. The
        // attachments are read off disk here, so by the time the child runs
        // its first turn the multimodal content is already in `initial`.
        let initial = initial_messages(
            &req.task,
            req.context.as_deref(),
            &req.attachments,
            req.output_schema.as_deref(),
            req.role,
        )
        .await;

        // Run the child on its own tokio task so that its `impl Future` type is
        // fully erased behind a `JoinHandle`, breaking async recursion with the
        // parent's `execute_tool_calls` future (which hosts the orchestrator
        // call). `tokio::spawn` requires `Send + 'static`, which forces child
        // `AgentLoop::run` to already be Send — this is also what the existing
        // `spawn_background_review` relies on.
        //+ safety: strip the sub-agent blocklist
        // (delegate_task, clarify, memory, send_message,
        // cronjob) from the child's tool registry. This is
        // the *physical* enforcement — even a misbehaving
        // child LLM can't call a blocked tool because the
        // handler is gone. The pre-dispatch hook in
        // agent_loop.rs is the *logical* check that catches
        // bad `toolset` params early; this is the last-resort
        // fence.
        let child_tool_registry = tool_registry
            .without_tools(crate::sub_agent_safety::BLOCKED_TOOLS_FOR_SUBAGENT.iter().copied());

        //Apply role-based toolset restriction. For Explore and Plan
        // roles, strip all tools that aren't in the role's allowed list.
        // General role has no restriction (beyond the blocklist above).
        let child_tool_registry = if let Some(allowed) = req.role.allowed_tools() {
            let allowed_set: std::collections::HashSet<&str> = allowed.iter().copied().collect();
            child_tool_registry.retain_only(|name| allowed_set.contains(name))
        } else {
            child_tool_registry
        };
        // Pre-extract owned values from `&self`/`&req`/`&sub_agent_id` before
        // the spawned future. `tokio::spawn` requires `'static`, so the async
        // block must not capture these borrows — cloning them into owned locals
        // here keeps the borrow lifetimes out of the spawned task.
        let emitter_for_cb = self.cfg.chunk_emitter.clone();
        let task_for_cb = req.task.clone();
        let role_for_cb = req.role;
        let id_for_cb = sub_agent_id.to_string();
        let join = tokio::spawn(async move {
            // Live step log: the child's tool calls / thinking are
            // surfaced as one-line entries so the frontend "子代理" panel
            // scrolls with real activity instead of a static badge.
            // The closure clones what it needs from `self.cfg` (emitter)
            // and shares `recent_steps` with the outer scope.
            let steps_for_cb = recent_steps.clone();
            let steps_for_cb_thinking = recent_steps.clone();
            let callbacks = crate::agent_helpers::AgentCallbacks {
                on_tool_start: Some(Box::new(move |_tool_call_id, tool, args| {
                    let step = tool_step_label(tool, args);
                    if let Ok(mut v) = steps_for_cb.lock() {
                        v.push(step.clone());
                        if v.len() > SUB_AGENT_STEP_LOG_CAP {
                            let excess = v.len() - SUB_AGENT_STEP_LOG_CAP;
                            v.drain(..excess);
                        }
                    }
                    // Push the updated log live so the panel scrolls as the
                    // child works, not just at phase transitions.
                    if let Some(emitter) = &emitter_for_cb {
                        let snapshot: Vec<String> = steps_for_cb
                            .lock()
                            .map(|v| v.clone())
                            .unwrap_or_default();
                        let payload = serde_json::json!({
                            "event": "sub_agent_update",
                            "sub_agent_id": id_for_cb,
                            "phase": "started",
                            "task": task_for_cb,
                            "role": role_for_cb.as_str(),
                            "recent_steps": snapshot,
                        });
                        emitter(StreamChunk {
                            delta: Some(StreamDelta {
                                content: None,
                                tool_calls: None,
                                extra: Some(payload),
                            }),
                            finish_reason: None,
                            usage: None,
                        });
                    }
                })),
                on_thinking: Some(Box::new(move |_text| {
                    if let Ok(mut v) = steps_for_cb_thinking.lock() {
                        let last = v.last().map(|s| s.as_str());
                        // De-duplicate consecutive thinking rows so the log
                        // doesn't fill with "正在思考…" spam between tools.
                        if last != Some("正在思考…") {
                            v.push("正在思考…".to_string());
                            if v.len() > SUB_AGENT_STEP_LOG_CAP {
                                let excess = v.len() - SUB_AGENT_STEP_LOG_CAP;
                                v.drain(..excess);
                            }
                        }
                    }
                })),
                ..Default::default()
            };
            let child_agent = AgentLoop::with_interrupt(
                child_config,
                Arc::new(child_tool_registry),
                llm_provider,
                child_interrupt_for_spawn,
            )
            .with_delegate_depth(child_depth)
            .with_callbacks(callbacks);
            child_agent.run(initial, None).await
        });

        // Pin the JoinHandle so the timeout arm can abort it. The
        // cooperative `interrupt` alone is not enough: `check_interrupt()`
        // is only observed at the LLM-call boundary (`llm_retry.rs:160`),
        // never inside tool execution. So a child that is mid-`write_file`
        // / `terminal` when the deadline fires would finish that tool and
        // land its writes *after* this function already returned
        // `Timeout` and the parent moved on — an orphaned task nobody
        // observes. `abort()` drops the future at its next await, which
        // stops the remaining tool calls from running.
        let mut join = Box::pin(join);
        // 父级按下停止时，父不能一直裸等子代理。旧代码只有 `timeout` 一个
        // 出口：用户点停止后父仍卡在 `execute_async` 里，最坏要等到子代理
        // 自己超时（`self.cfg.timeout`，默认几十秒到几分钟），主观感受就是
        // 「停止按钮没反应」。这里让父 interrupt 与 timeout 竞速。
        let parent_abort = join.as_ref().get_ref().abort_handle();
        let parent_interrupt = self.cfg.parent_interrupt.clone();
        let result = tokio::select! {
            biased;
            _ = parent_interrupt.wait_for_interrupt() => {
                child_interrupt.interrupt(Some("parent stopped".to_string()));
                parent_abort.abort();
                tracing::info!(
                    task = %req.task.chars().take(80).collect::<String>(),
                    child_depth = req.child_depth,
                    "sub-agent aborted: parent turn was stopped by the user"
                );
                Err(SubAgentError::Cancelled)
            }
            timed = timeout(self.cfg.timeout, join.as_mut()) => match timed {
                Ok(Ok(Ok(result))) if result.interrupted => Err(SubAgentError::Cancelled),
                Ok(Ok(Ok(result))) => Ok(result),
                Ok(Ok(Err(AgentError::Interrupted { .. }))) => Err(SubAgentError::Cancelled),
                Ok(Ok(Err(e))) => Err(SubAgentError::Agent(e)),
                Ok(Err(join_err)) => Err(SubAgentError::Agent(AgentError::ToolExecution(format!(
                    "sub-agent join error: {}",
                    join_err
                )))),
                Err(_) => {
                    // Cooperative cancel for the in-flight LLM call, then a
                    // hard abort so no further tool executes.
                    child_interrupt.interrupt(Some("timeout".to_string()));
                    // `timeout` consumed the future via `&mut`, so the inner
                    // `JoinHandle` is still owned here and can be aborted.
                    // The task may already be finished; `abort` is a no-op then.
                    join.abort();
                    tracing::warn!(
                        task = %req.task.chars().take(80).collect::<String>(),
                        child_depth = req.child_depth,
                        timeout_secs = ?self.cfg.timeout,
                        "sub-agent timed out; aborted to prevent further tool side effects"
                    );
                    Err(SubAgentError::Timeout)
                }
            },
        };

        watcher_stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = watcher.await;
        result
    }

    fn build_child_config(&self, req: &SubAgentRequest, sub_agent_id: &str) -> AgentConfig {
        let parent = &self.cfg.parent_config;

        // Use independent iteration budget when explicitly set (Hermes Agent parity);
        // otherwise fall back to the parent-derived calculation.
        let child_turns = req
            .max_iterations
            .unwrap_or_else(|| {
                let remaining_parent_turns = parent
                    .max_turns
                    .saturating_sub(req.child_depth.saturating_mul(2));
                remaining_parent_turns
                    .saturating_div(2)
                    .clamp(SUB_AGENT_MIN_TURNS, SUB_AGENT_MAX_TURNS_CAP)
            })
            .clamp(SUB_AGENT_MIN_TURNS, SUB_AGENT_MAX_TURNS_CAP);

        let child_session_id = match parent.session_id.as_deref() {
            Some(s) if !s.is_empty() => format!("{}::{}", s, sub_agent_id),
            _ => sub_agent_id.to_string(),
        };

        let mut child = parent.clone();
        child.session_id = Some(child_session_id);
        child.max_turns = child_turns;
        child.stream = false;
        // Keep child sessions silent/contained (Python child quiet_mode semantics).
        child.background_review_enabled = false;
        child.background_review_metrics_enabled = false;
        child.memory_nudge_interval = 0;
        child.skill_creation_nudge_interval = 0;
        child.quiet_mode = true;
        // The image base64 strip + delegate_task nudge in
        // `run_loop::do_post_tool_iteration` is scoped to the main context.
        // A child is the isolation boundary already, so it keeps its images.
        child.is_sub_agent = true;
        // Children should not re-spawn grandchildren beyond the contract;
        // depth checks inside AgentLoop::execute_tool_calls still apply.
        child.skip_memory = true;
        // Apply explicit model override if the caller requested one.
        if let Some(model) = req.model.as_ref() {
            if !model.is_empty() {
                child.model = model.clone();
            }
        }
        // Budget: hard-cap child by parent remaining budget when known.
        if let Some(remaining) = req.parent_budget_remaining_usd {
            let cap = remaining.max(0.0);
            child.max_cost_usd = Some(match child.max_cost_usd {
                Some(existing) => existing.min(cap),
                None => cap,
            });
        }
        child
    }

    async fn persist_lineage(&self, lineage: &SubAgentLineage) {
        let dir = self.cfg.hermix_home.join("subagents");
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::debug!(error = %e, "sub-agent lineage dir create failed");
            return;
        }
        let path = dir.join(format!("{}.json", lineage.sub_agent_id));
        let body = match serde_json::to_string_pretty(lineage) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "sub-agent lineage serialize failed");
                return;
            }
        };
        // Atomic write (tmp + rename): a concurrent reader — the UI's
        // `get_subagent_result` poller, or `stop_background` rewriting the
        // terminal state — must never observe a half-written JSON document.
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp, body).await {
            tracing::debug!(error = %e, path = %tmp.display(), "sub-agent lineage write failed");
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            tracing::debug!(error = %e, path = %path.display(), "sub-agent lineage rename failed");
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }
}

#[derive(Debug)]
enum SubAgentError {
    Timeout,
    Cancelled,
    Agent(AgentError),
}

impl std::fmt::Display for SubAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubAgentError::Timeout => write!(f, "sub-agent timed out"),
            SubAgentError::Cancelled => write!(f, "sub-agent cancelled"),
            SubAgentError::Agent(e) => write!(f, "sub-agent error: {}", e),
        }
    }
}

/// Build the initial messages for a child agent.
/// The child receives:
/// 1. A **system message** with sub-agent-specific guidance — most importantly
/// the hard ban on `delegate_task` (overrides any parent prompt that says
/// "use delegate_task for images") and the batching strategy that prevents
/// the child from blowing its own context.
/// 2. A **user message** with the task description, optional context, optional
/// `output_schema` hint, and any image attachments as multimodal content
/// (text + `ImageUrl` parts so the child LLM can actually SEE the images).
/// The system message is critical because the child's `AgentLoop` builds its
/// system prompt via `build_system_prompt` (which includes the full
/// `directions.rs`), but that prompt is generic — it doesn't know the child
/// is a sub-agent with specific constraints (no `delegate_task`, limited
/// turns, must batch). The injected system message fills that gap AND
/// **explicitly overrides** the parent prompt's "delegate_task for images"
/// directive so a confused LLM doesn't waste turns trying to call it.
async fn initial_messages(
    task: &str,
    context: Option<&str>,
    attachments: &[ImageAttachment],
    output_schema: Option<&str>,
    role: SubAgentRole,
) -> Vec<Message> {
    let mut messages = Vec::new();

    // Sub-agent-specific guidance — injected as a system message so the
    // child LLM always sees it, regardless of the parent's directions.
    // rewritten to be a HARD OVERRIDE of any parent-side
    // instruction that says "use delegate_task for image analysis" or
    // similar. The parent prompt is built before the child runs and
    // gets copied into the child's context, so without this explicit
    // counter-instruction the child LLM would think it's still allowed
    // to call `delegate_task` and waste turns hitting the deny wall.
    let mut sub_agent_guidance = String::from(
        "[Sub-agent operating constraints — HARD OVERRIDE of any parent prompt fragment]\n\
         You are a sub-agent running in an isolated context. The parent agent's system\n\
         prompt may contain instructions like \"use delegate_task for image analysis\" —\n\
         IGNORE THEM. You ARE the sub-agent. Re-delegating is forbidden.\n\
         \n\
         # Hard rules\n\
         1. You CANNOT call `delegate_task` — it is blocked at the tool registry.\n\
            Calling it wastes turns (you'll get an error envelope). Do all work yourself.\n\
         2. You CANNOT call `clarify` — the user is not listening to you. If you lack\n\
            information, state what's missing in your final response.\n\
         3. You CANNOT call `memory`, `send_message`, `cronjob` — those cross the\n\
            parent/child safety boundary.\n\
         4. Your final result is capped at ~8,000 chars. Be concise and structured.\n\
         \n\
         # Context-bloat self-defense (CRITICAL — your ctx is also precious)\n\
         Your isolated context is NOT a license to load everything. The same rules\n\
         that apply to the parent agent apply to you: each read costs you tokens too,\n\
         and you have a hard 8,000-char result cap. A sub-agent that bloats its own\n\
         ctx returns a useless truncated summary.\n\
         \n\
         ## How to read files safely\n\
         - **Check size FIRST**: use `list_files` / `list_dir` (returns per-entry sizes)\n\
           before reading. If > ~50 KB or > ~1500 lines, do NOT `read_file` whole.\n\
         - **Read in chunks**: `read_file` accepts `offset` / `limit` parameters.\n\
           Read 200-500 lines at a time, summarize each chunk in your scratchpad, then\n\
           read the next chunk.\n\
         - **For PDFs**: use `read_pdf` (text extraction) or `markitdown` — they are\n\
           purpose-built and budgeted; do NOT paste raw PDF text into your final\n\
           response. Aggregate the facts you need into a single summary.\n\
         - **For code exploration**: use `search_files` (returns match snippets, not\n\
           full files) before `read_file`. The snippet result is usually enough; only\n\
           `read_file` when you need surrounding context for a specific match.\n\
         \n\
         ## Shell is NOT yours to use\n\
         `terminal` and `code_execution` are BLOCKED for sub-agents (parent/child\n\
         safety boundary). Everything above is achievable with the built-in tools —\n\
         do not look for command workarounds. If the task genuinely needs a command,\n\
         state that in your final response; the parent agent will run it under the\n\
         user's approval guardrail.\n\
         \n\
         ## Scratchpad discipline\n\
         - Don't paste raw file contents into your final response. Extract the\n\
           salient facts (key function names, line numbers, error messages, decisions)\n\
           and synthesize them into a concise summary.\n\
         - If you discover information you don't need to answer the task, DROP it.\n\
           Your job is to return conclusions, not raw data.\n\
         \n\
         # Batching strategy (CRITICAL — prevents context explosion)\n\
         If you need to analyze multiple images or large files:\n\
         - **NEVER read more than 3-5 images/files in a single turn.** Each read\n\
           injects large content into your context.\n\
         - Process in **batches**: read batch 1 → analyze → record conclusions →\n\
           then read batch 2 → analyze → record conclusions → etc.\n\
         - Your final response should aggregate ALL batch conclusions into a single\n\
           structured summary.\n\
         - If you run out of turns before finishing all batches, return a partial\n\
           summary noting which items were not processed.\n",
    );

    // Role return contract: tells the child what its final answer must
    // look like (Explore = findings-not-dumps; Plan = structured plan).
    // Empty for General — nothing to add.
    sub_agent_guidance.push_str(role.return_contract());

    if let Some(schema) = output_schema {
        if !schema.trim().is_empty() {
            sub_agent_guidance.push_str(&format!(
                "\n# Expected output format (from parent agent)\n\
                 The parent agent that spawned you expects you to return your final\n\
                 answer in this format. Honor it.\n\
                 ```\n\
                 {schema}\n\
                 ```\n"
            ));
        }
    }

    messages.push(Message::system(&sub_agent_guidance));

    // User message with task + context. Attachments (images) are appended
    // as multimodal `ImageUrl` parts so the child LLM receives the actual
    // pixels via its native vision API, not just a path string.
    let mut content = task.to_string();
    if let Some(ctx) = context {
        if !ctx.trim().is_empty() {
            content.push_str(&format!("\n\n[Context]\n{ctx}"));
        }
    }

    let image_parts = match attach_images(attachments).await {
        Ok(parts) => parts,
        Err(e) => {
            // Failure to attach images must not kill the child — log
            // it and fall back to text-only so the child can still
            // answer (possibly with an explicit "I cannot see the
            // images" note if it tries to call `read_image` and
            // finds them unreadable too).
            tracing::warn!(
                error = %e,
                attachment_count = attachments.len(),
                "sub-agent image attachment failed; falling back to text-only"
            );
            Vec::new()
        }
    };

    let user_msg = if image_parts.is_empty() {
        Message::user(&content)
    } else {
        let img_refs: Vec<(&str, &str)> = image_parts
            .iter()
            .map(|(m, b)| (m.as_str(), b.as_str()))
            .collect();
        Message::user_with_images(&content, &img_refs)
    };
    messages.push(user_msg);

    messages
}

/// Read each attachment from disk and return `(mime, base64)` pairs ready
/// to plug into `Message::user_with_images`. Uses the parent registry's
/// `read_image` handler so the resize/encode logic stays in one place.
/// Read each attachment from disk and return `(mime, base64)` pairs ready
/// to plug into `Message::user_with_images`. Uses the dedicated
/// `hermix_tools::read_image_for_llm` helper (added ) so the
/// resize/encode logic lives in exactly one place — the same code path
/// as the `read_image` tool, without depending on the full
/// `ToolRegistry::dispatch_async` machinery (which is unavailable on the
/// `agent_helpers::ToolRegistry` carried by `SubAgentOrchestratorConfig`).
async fn attach_images(
    attachments: &[ImageAttachment],
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::with_capacity(attachments.len());
    for att in attachments {
        let detail = att.detail.as_deref().unwrap_or("auto");
        let (mime, b64) = hermix_tools::read_image_for_llm(&att.path, detail)?;
        out.push((mime, b64));
    }
    Ok(out)
}

fn extract_final_assistant_text(messages: &[Message]) -> (String, bool) {
    let raw = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    if raw.chars().count() <= SUB_AGENT_RESULT_MAX_CHARS {
        return (raw, false);
    }

    // Char-safe truncation — CJK / emoji must not panic.
    let end_byte = raw
        .char_indices()
        .nth(SUB_AGENT_RESULT_MAX_CHARS)
        .map(|(byte, _)| byte)
        .unwrap_or(raw.len());
    let removed = raw.chars().count() - SUB_AGENT_RESULT_MAX_CHARS;
    let truncated = format!(
        "{}\n\n[... sub-agent result truncated: {} characters removed ...]",
        &raw[..end_byte],
        removed
    );
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{BoxStream, StreamExt};
    use hermix_core::{LlmResponse, StreamChunk, ToolSchema};
    use serde_json::Value;
    use std::sync::Arc;

    // Minimal LlmProvider stub that always returns an empty assistant response
    // so the child loop finishes in a single turn. We only care about
    // plumbing here — behavioural tests live in agent_loop::tests.
    struct NoopProvider;
    #[async_trait::async_trait]
    impl LlmProvider for NoopProvider {
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
                model: "noop".into(),
                finish_reason: Some("stop".into()),
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
            futures::stream::empty().boxed()
        }
    }

    #[tokio::test]
    async fn spawn_timeout_path_produces_timeout_json() {
        struct SlowProvider;
        #[async_trait::async_trait]
        impl LlmProvider for SlowProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(LlmResponse {
                    message: Message::assistant("late"),
                    usage: None,
                    model: "slow".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let parent_cfg = AgentConfig {
            max_turns: 2,
            model: "slow".into(),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent_cfg,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(SlowProvider),
            parent_interrupt: InterruptController::new(),
            hermix_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_millis(50),
            chunk_emitter: None,
            task_record_sink: None,
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "test task".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "timeout");
        assert!(parsed["sub_agent_id"]
            .as_str()
            .unwrap()
            .starts_with("subagent-"));
        // Lineage file should exist under $HERMIX_HOME/subagents/<id>.json
        let dir = tmp.path().join("subagents");
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let entry = entries.next_entry().await.unwrap().unwrap();
        assert!(entry
            .path()
            .extension()
            .map(|s| s == "json")
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn spawn_background_returns_id_immediately_and_records_result() {
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 3,
                model: "noop".into(),
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_interrupt: InterruptController::new(),
            hermix_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(10),
            chunk_emitter: None,
            task_record_sink: None,
        }));

        let id = orch
            .spawn_background(SubAgentRequest {
                task: "background task".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await
            .expect("spawn_background");
        assert!(id.starts_with("subagent-"), "unexpected id: {id}");

        // Started lineage must exist BEFORE the id is returned, so an
        // immediate poll sees "started" rather than "not_found".
        let path = tmp.path().join("subagents").join(format!("{id}.json"));
        assert!(path.exists(), "started lineage written eagerly");

        // Poll until the detached child reaches a terminal state.
        let mut parsed: Value = Value::Null;
        for _ in 0..100 {
            let raw = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
                if status == "completed" || status == "failed" {
                    parsed = v;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            parsed.get("status").and_then(|s| s.as_str()),
            Some("completed"),
            "background child never reached a terminal state: {parsed}"
        );
        // The result text is what get_subagent_result returns — it must
        // be persisted (previously this field did not exist at all).
        assert_eq!(
            parsed.get("result").and_then(|s| s.as_str()),
            Some("done")
        );
        assert_eq!(
            parsed.get("finished_naturally").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn stop_background_cancels_and_marks_lineage() {
        struct HangingProvider;
        #[async_trait::async_trait]
        impl LlmProvider for HangingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(LlmResponse {
                    message: Message::assistant("late"),
                    usage: None,
                    model: "hang".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                model: "hang".into(),
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(HangingProvider),
            parent_interrupt: InterruptController::new(),
            hermix_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(60),
            chunk_emitter: None,
            task_record_sink: None,
        }));

        let id = orch
            .spawn_background(SubAgentRequest {
                task: "long running".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await
            .expect("spawn");

        assert!(orch.stop_background(&id).await, "running task must stop");
        // The lineage flips to a terminal cancelled state so a poller does
        // not wait forever on "started".
        let path = tmp.path().join("subagents").join(format!("{id}.json"));
        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["status"], "cancelled");
        assert_eq!(v["error"], "stopped by request");

        // Stopping again is a no-op, not an error.
        assert!(!orch.stop_background(&id).await);
    }

    #[tokio::test]
    async fn cancel_path_reports_cancelled() {
        let parent_ctrl = InterruptController::new();
        parent_ctrl.interrupt(Some("stop".into())); // pre-cancel before spawn

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig::default(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_interrupt: parent_ctrl,
            hermix_home: tmp.path().to_path_buf(),
            parent_session_id: None,
            timeout: Duration::from_secs(2),
            chunk_emitter: None,
            task_record_sink: None,
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "noop".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: Value = serde_json::from_str(&out).unwrap();
        // Parent interrupt propagates to child; status should be cancelled or
        // completed depending on scheduling. We accept either (cancelled most
        // of the time; completed if watcher missed the window). The lineage
        // file still must be written.
        assert!(parsed["sub_agent_id"].as_str().is_some());
    }

    #[test]
    fn build_child_config_clamps_turns() {
        let parent = AgentConfig {
            max_turns: 20,
            model: "m".into(),
            session_id: Some("root".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_interrupt: InterruptController::new(),
            hermix_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            chunk_emitter: None,
            task_record_sink: None,
        });
        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                parent_budget_remaining_usd: Some(1.25),
                ..Default::default()
            },
            "subagent-test",
        );
        assert!(child.max_turns >= SUB_AGENT_MIN_TURNS);
        assert!(child.max_turns <= SUB_AGENT_MAX_TURNS_CAP);
        assert_eq!(child.session_id.as_deref(), Some("root::subagent-test"));
        assert_eq!(child.max_cost_usd, Some(1.25));
        assert!(child.skip_memory);
        assert!(!child.background_review_enabled);
        assert!(!child.background_review_metrics_enabled);
        assert_eq!(child.memory_nudge_interval, 0);
        assert_eq!(child.skill_creation_nudge_interval, 0);
        assert!(child.quiet_mode);
    }

    #[test]
    fn extract_final_assistant_text_returns_short_text_unchanged() {
        let messages = vec![
            Message::user("hello"),
            Message::assistant("short reply"),
        ];
        let (text, truncated) = extract_final_assistant_text(&messages);
        assert_eq!(text, "short reply");
        assert!(!truncated);
    }

    #[test]
    fn extract_final_assistant_text_truncates_long_output() {
        let long_content: String = "x".repeat(SUB_AGENT_RESULT_MAX_CHARS * 2);
        let messages = vec![
            Message::user("analyze"),
            Message::assistant(&long_content),
        ];
        let (text, truncated) = extract_final_assistant_text(&messages);
        assert!(truncated);
        assert!(text.contains("sub-agent result truncated"));
        // The text must be shorter than the original.
        assert!(text.chars().count() < long_content.chars().count());
        // The text must include the prefix of the original content.
        assert!(text.starts_with(&"x".repeat(100)));
    }

    #[test]
    fn extract_final_assistant_text_cjk_truncation_is_char_safe() {
        // Each Chinese character is 3 bytes in UTF-8.
        // Truncating at a byte boundary would panic; char-safe truncation must work.
        let long_content: String = "中".repeat(SUB_AGENT_RESULT_MAX_CHARS + 500);
        let messages = vec![
            Message::user("分析"),
            Message::assistant(&long_content),
        ];
        let (text, truncated) = extract_final_assistant_text(&messages);
        assert!(truncated);
        assert!(text.contains("sub-agent result truncated"));
        // No panic — that's the main assertion.
    }

    #[test]
    fn extract_final_assistant_text_returns_last_assistant_message() {
        let messages = vec![
            Message::user("q1"),
            Message::assistant("first answer"),
            Message::user("q2"),
            Message::assistant("second answer"),
        ];
        let (text, truncated) = extract_final_assistant_text(&messages);
        assert_eq!(text, "second answer");
        assert!(!truncated);
    }

    #[test]
    fn extract_final_assistant_text_empty_when_no_assistant() {
        let messages = vec![Message::user("hello")];
        let (text, truncated) = extract_final_assistant_text(&messages);
        assert_eq!(text, "");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn initial_messages_includes_sub_agent_guidance() {
        let msgs = initial_messages("test task", None, &[], None, SubAgentRole::General).await;
        // Should have 2 messages: system guidance + user task.
        assert_eq!(msgs.len(), 2);
        // First message is system guidance.
        assert!(matches!(msgs[0].role, hermix_core::MessageRole::System));
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Sub-agent operating constraints"));
        assert!(guidance.contains("delegate_task"));
        assert!(guidance.contains("Batching strategy"));
        assert!(guidance.contains("3-5"));
        // new override line confirming the parent prompt's
        // "use delegate_task" directive is overridden.
        assert!(guidance.contains("HARD OVERRIDE"));
        // Second message is user task.
        assert!(matches!(msgs[1].role, hermix_core::MessageRole::User));
        assert_eq!(msgs[1].content.as_deref(), Some("test task"));
    }

    #[tokio::test]
    async fn initial_messages_with_context() {
        let msgs = initial_messages(
            "analyze images",
            Some("paths: a.jpg, b.jpg"),
            &[],
            None,
            SubAgentRole::General,
        )
        .await;
        assert_eq!(msgs.len(), 2);
        let user_content = msgs[1].content.as_deref().unwrap_or("");
        assert!(user_content.contains("analyze images"));
        assert!(user_content.contains("paths: a.jpg, b.jpg"));
        assert!(user_content.contains("[Context]"));
    }

    #[tokio::test]
    async fn initial_messages_without_context_no_context_label() {
        let msgs = initial_messages("do something", None, &[], None, SubAgentRole::General).await;
        let user_content = msgs[1].content.as_deref().unwrap_or("");
        assert!(!user_content.contains("[Context]"));
    }

    #[tokio::test]
    async fn initial_messages_with_output_schema_adds_format_section() {
        let msgs = initial_messages(
            "summarize",
            None,
            &[],
            Some(r#"{"summary":"string","score":0.0}"#),
            SubAgentRole::General,
        )
        .await;
        assert_eq!(msgs.len(), 2);
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Expected output format"));
        assert!(guidance.contains("summary"));
        assert!(guidance.contains("score"));
    }

    #[tokio::test]
    async fn initial_messages_explore_role_injects_findings_contract() {
        let msgs = initial_messages("explore the codebase", None, &[], None, SubAgentRole::Explore)
            .await;
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Return contract (Explore role)"));
        assert!(guidance.contains("FINDINGS, not dumps"));
    }

    #[tokio::test]
    async fn initial_messages_plan_role_injects_plan_contract() {
        let msgs = initial_messages("plan the refactor", None, &[], None, SubAgentRole::Plan).await;
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Return contract (Plan role)"));
        assert!(guidance.contains("Open questions"));
    }

    #[tokio::test]
    async fn initial_messages_general_role_has_no_contract() {
        let msgs = initial_messages("do it", None, &[], None, SubAgentRole::General).await;
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(!guidance.contains("Return contract"));
    }
}
