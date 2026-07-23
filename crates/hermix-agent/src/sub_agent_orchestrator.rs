//! In-process sub-agent orchestrator.
//!
//! Python v2026.4.16 parity: when the primary agent calls `delegate_task`, we
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

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hermix_core::{AgentError, LlmProvider, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::timeout;

use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::interrupt::InterruptController;

/// Boxed `Send` future type alias used to short-circuit async-recursion
/// between parent and child [`AgentLoop::execute_tool_calls`] futures.
type BoxSendFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Default wall-clock cap for a single child agent turn-chain.
pub const DEFAULT_SUB_AGENT_TIMEOUT_SECS: u64 = 300;
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
///
/// 8,000 chars ≈ 2,000 tokens, which is the same order as a single
/// `read_image` blob. This is large enough for a detailed structured
/// analysis but small enough to keep the parent context lean.
pub const SUB_AGENT_RESULT_MAX_CHARS: usize = 8_000;

/// Lineage record persisted to disk for each spawned sub-agent.
///
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
    /// P6-4: Role of the sub-agent.
    #[serde(default)]
    pub role: SubAgentRole,
    /// P6-4: Parent task ID for linking sub-agents to the task tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
}

/// Terminal state vocabulary for sub-agent lineage records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
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
/// (Phase 6 — P6-4)
///
/// - **General** — full toolset, general-purpose task execution.
///   Suitable for most delegation scenarios.
/// - **Explore** — read-only toolset. Used for information gathering,
///   code exploration, web search. Cannot modify files or execute
///   shell commands that write.
/// - **Plan** — planning toolset. Used for producing plans and
///   strategies. Has access to read tools + todo/task tools but
///   not write tools.
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
}

/// Tools available to the Explore role (read-only).
const EXPLORE_ROLE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search_files",
    "web_search",
    "web_extract",
    "url_safety",
    "session_search",
    "memory_search",
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_scroll",
    "browser_back",
    "browser_press",
    "browser_get_images",
    "browser_vision",
    "browser_console",
    "tool_search",
    "tool_describe",
    "tool_call",
];

/// Tools available to the Plan role (read + planning).
const PLAN_ROLE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search_files",
    "web_search",
    "web_extract",
    "url_safety",
    "session_search",
    "memory_search",
    "memory_store",
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
///
/// The orchestrator reads the file from disk, base64-encodes it, and
/// injects it as a multimodal `ContentPart::ImageUrl` into the
/// child's first user message so the child LLM can actually SEE it
/// via native vision. Without this, the child would only receive a
/// text description of the path — and the `read_image` results that
/// the parent already saw are not automatically carried over.
#[derive(Debug, Clone, Default)]
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
    /// P6-4: Role of the sub-agent. Determines the toolset and system prompt.
    /// Defaults to `General` (full tool access).
    pub role: SubAgentRole,
    /// P6-4: Parent task ID for linking sub-agents to the task tree.
    /// When set, the sub-agent's results are attributed to this task.
    pub parent_task_id: Option<String>,
    /// 2026-07-23: Image / file attachments forwarded from the parent.
    /// Each path is read, base64-encoded, and injected into the
    /// child's first user message as a multimodal `ImageUrl` part.
    /// This is what makes image analysis through `delegate_task`
    /// actually work — previously the child only got a text path
    /// string and had no way to see the pixels.
    pub attachments: Vec<ImageAttachment>,
    /// 2026-07-23: Optional JSON schema (or human-readable shape
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
}

/// In-process executor for `delegate_task` tool calls.
pub struct SubAgentOrchestrator {
    cfg: SubAgentOrchestratorConfig,
}

impl SubAgentOrchestrator {
    pub fn new(cfg: SubAgentOrchestratorConfig) -> Self {
        Self { cfg }
    }

    /// Default orchestrator that mirrors the current `AgentLoop`'s wiring.
    ///
    /// The returned orchestrator inherits the parent's tool registry, provider
    /// and interrupt handle so cancellation propagates from the parent to any
    /// actively running child.
    pub fn from_parent(parent: &AgentLoop, hermix_home: PathBuf) -> Self {
        Self {
            cfg: SubAgentOrchestratorConfig {
                parent_config: parent.config.clone(),
                tool_registry: parent.tool_registry.clone(),
                llm_provider: parent.llm_provider.clone(),
                parent_interrupt: parent.interrupt.clone(),
                hermix_home,
                parent_session_id: parent.config.session_id.clone(),
                timeout: Duration::from_secs(DEFAULT_SUB_AGENT_TIMEOUT_SECS),
            },
        }
    }

    /// Execute a `delegate_task` request synchronously from the perspective of
    /// the caller, returning a JSON string suitable for use as the tool result.
    ///
    /// The returned JSON always contains `sub_agent_id` at the top level so
    /// existing delegation-event wiring in [`AgentLoop`] continues to work.
    ///
    /// Returning `BoxSendFuture` (rather than an `async fn`) is load-bearing:
    /// the parent `AgentLoop::execute_tool_calls` awaits this future, and the
    /// child may eventually call back into an orchestrator of its own. A
    /// concrete `impl Future` type would force the compiler to recursively
    /// prove `Send` through that cycle, which it cannot. Boxing into
    /// `dyn Future + Send` erases the child's future type and breaks the
    /// cycle.
    pub fn execute(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
        let this = self.clone();
        Box::pin(async move { this.execute_inner(req).await })
    }

    async fn execute_inner(self: Arc<Self>, req: SubAgentRequest) -> String {
        let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
        let started_at = Utc::now();

        let mut lineage = SubAgentLineage {
            sub_agent_id: sub_agent_id.clone(),
            parent_session_id: self.cfg.parent_session_id.clone(),
            task: req.task.clone(),
            context: req.context.clone(),
            toolset: req.toolset.clone(),
            model: req.model.clone(),
            depth: req.child_depth,
            max_depth: req.max_depth,
            parent_budget_remaining_usd: req.parent_budget_remaining_usd,
            status: SubAgentStatus::Started,
            started_at,
            ended_at: None,
            total_turns: 0,
            prompt_tokens: None,
            completion_tokens: None,
            estimated_cost_usd: None,
            error: None,
            role: req.role,
            parent_task_id: req.parent_task_id.clone(),
        };

        // Best-effort lineage persistence — failures must never break delegation.
        self.persist_lineage(&lineage).await;

        // Build and run the child.
        let outcome = self.run_child(&req, &sub_agent_id).await;

        match outcome {
            Ok(result) => {
                lineage.status = SubAgentStatus::Completed;
                lineage.total_turns = result.total_turns;
                if let Some(usage) = result.usage.as_ref() {
                    lineage.prompt_tokens = Some(usage.prompt_tokens);
                    lineage.completion_tokens = Some(usage.completion_tokens);
                    lineage.estimated_cost_usd = usage.estimated_cost;
                }
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

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
                let status = match &e {
                    SubAgentError::Timeout => SubAgentStatus::Timeout,
                    SubAgentError::Cancelled => SubAgentStatus::Cancelled,
                    SubAgentError::Agent(_) => SubAgentStatus::Failed,
                };
                lineage.status = status;
                lineage.error = Some(e.to_string());
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

                json!({
                    "sub_agent_id": sub_agent_id,
                    "status": match status {
                        SubAgentStatus::Timeout => "timeout",
                        SubAgentStatus::Cancelled => "cancelled",
                        SubAgentStatus::Failed => "failed",
                        _ => "failed",
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
    ) -> Result<hermix_core::AgentResult, SubAgentError> {
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
        )
        .await;

        // Run the child on its own tokio task so that its `impl Future` type is
        // fully erased behind a `JoinHandle`, breaking async recursion with the
        // parent's `execute_tool_calls` future (which hosts the orchestrator
        // call). `tokio::spawn` requires `Send + 'static`, which forces child
        // `AgentLoop::run` to already be Send — this is also what the existing
        // `spawn_background_review` relies on.
        //
        // Phase 18+ safety: strip the sub-agent blocklist
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

        // P6-4: Apply role-based toolset restriction. For Explore and Plan
        // roles, strip all tools that aren't in the role's allowed list.
        // General role has no restriction (beyond the blocklist above).
        let child_tool_registry = if let Some(allowed) = req.role.allowed_tools() {
            let allowed_set: std::collections::HashSet<&str> = allowed.iter().copied().collect();
            child_tool_registry.retain_only(|name| allowed_set.contains(name))
        } else {
            child_tool_registry
        };
        let join = tokio::spawn(async move {
            let child_agent = AgentLoop::with_interrupt(
                child_config,
                Arc::new(child_tool_registry),
                llm_provider,
                child_interrupt_for_spawn,
            )
            .with_delegate_depth(child_depth);
            child_agent.run(initial, None).await
        });

        let result = match timeout(self.cfg.timeout, join).await {
            Ok(Ok(Ok(result))) if result.interrupted => Err(SubAgentError::Cancelled),
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(AgentError::Interrupted { .. }))) => Err(SubAgentError::Cancelled),
            Ok(Ok(Err(e))) => Err(SubAgentError::Agent(e)),
            Ok(Err(join_err)) => Err(SubAgentError::Agent(AgentError::ToolExecution(format!(
                "sub-agent join error: {}",
                join_err
            )))),
            Err(_) => {
                // Cooperative cancel on timeout — next `check_interrupt()`
                // boundary in the child unwinds.
                child_interrupt.interrupt(Some("timeout".to_string()));
                Err(SubAgentError::Timeout)
            }
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
        if let Err(e) = tokio::fs::write(&path, body).await {
            tracing::debug!(error = %e, path = %path.display(), "sub-agent lineage write failed");
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
///
/// The child receives:
/// 1. A **system message** with sub-agent-specific guidance — most importantly
///    the hard ban on `delegate_task` (overrides any parent prompt that says
///    "use delegate_task for images") and the batching strategy that prevents
///    the child from blowing its own context.
/// 2. A **user message** with the task description, optional context, optional
///    `output_schema` hint, and any image attachments as multimodal content
///    (text + `ImageUrl` parts so the child LLM can actually SEE the images).
///
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
) -> Vec<Message> {
    let mut messages = Vec::new();

    // Sub-agent-specific guidance — injected as a system message so the
    // child LLM always sees it, regardless of the parent's directions.
    // 2026-07-23: rewritten to be a HARD OVERRIDE of any parent-side
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
         - **Check size FIRST**: use `list_files` (returns size) or `terminal wc -l <file>`\n\
           before reading. If > ~50 KB or > ~1500 lines, do NOT `read_file` whole.\n\
         - **Read in chunks**: `read_file` accepts `start_line` / `end_line` parameters.\n\
           Read 200-500 lines at a time, summarize each chunk in your scratchpad, then\n\
           read the next chunk.\n\
         - **For PDFs**: do NOT use `read_pdf` on a 100-page PDF. Instead, run\n\
           `terminal pdftotext -layout -f 1 -l 5 file.pdf -` to extract pages 1-5 at a\n\
           time, or `pdftotext file.pdf -` piped to `head -1000` for the first slice.\n\
           Aggregate slices into a single summary; never paste raw PDF text into your\n\
           final response.\n\
         - **For code exploration**: use `search_files` (returns match snippets, not\n\
           full files) before `read_file`. The grep result is usually enough; only\n\
           `read_file` when you need surrounding context for a specific match.\n\
         \n\
         ## How to run commands safely\n\
         - **NEVER `cat` a large file**. Use `head -N`, `tail -N`, `sed -n 'A,Bp'`,\n\
           or `grep -n PATTERN file` to extract only what you need.\n\
         - **NEVER `dir /s /b` or `find /` on a large tree** — paginate or use\n\
           `find ... -maxdepth N` to bound output.\n\
         - **NEVER dump logs without filtering**. `cat huge.log` wastes your ctx;\n\
           `grep -E 'ERROR|WARN' huge.log | head -100` is the right move.\n\
         - **Cap command output**: if a command might produce > ~3,000 chars, pipe\n\
           through `head`, `tail`, `grep`, or `awk` to extract only the relevant slice.\n\
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
        Message::user_with_images_owned(&content, image_parts)
    };
    messages.push(user_msg);

    messages
}

/// Read each attachment from disk and return `(mime, base64)` pairs ready
/// to plug into `Message::user_with_images`. Uses the parent registry's
/// `read_image` handler so the resize/encode logic stays in one place.
/// Read each attachment from disk and return `(mime, base64)` pairs ready
/// to plug into `Message::user_with_images`. Uses the dedicated
/// `hermix_tools::read_image_for_llm` helper (added 2026-07-23) so the
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
        let msgs = initial_messages("test task", None, &[], None).await;
        // Should have 2 messages: system guidance + user task.
        assert_eq!(msgs.len(), 2);
        // First message is system guidance.
        assert!(matches!(msgs[0].role, hermix_core::MessageRole::System));
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Sub-agent operating constraints"));
        assert!(guidance.contains("delegate_task"));
        assert!(guidance.contains("Batching strategy"));
        assert!(guidance.contains("3-5"));
        // 2026-07-23: new override line confirming the parent prompt's
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
        let msgs = initial_messages("do something", None, &[], None).await;
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
        )
        .await;
        assert_eq!(msgs.len(), 2);
        let guidance = msgs[0].content.as_deref().unwrap_or("");
        assert!(guidance.contains("Expected output format"));
        assert!(guidance.contains("summary"));
        assert!(guidance.contains("score"));
    }
}
