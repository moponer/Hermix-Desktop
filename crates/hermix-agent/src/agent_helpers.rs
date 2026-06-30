//! Standalone helper types and functions used by `agent_loop`.
//!
//! This module was extracted from `agent_loop.rs` (REFACTOR_PLAN §4, P2.2.5
//! continuation, 2026-06-30) so the 10k-line `agent_loop.rs` can focus on the
//! `impl AgentLoop` block. **No behavior change** — pure relocation.
//!
//! Items living here:
//! - `ToolEntry`, `ToolRegistry` (was agent_loop.rs:70-153)
//! - `TurnMetrics`, `EvolutionCounters` (was agent_loop.rs:557-578)
//! - `ToolDispatchDecision`, `AgentCallbacks` (was agent_loop.rs:584-693)
//! - `ErrorClass`, `classify_error`, `jittered_backoff`, `rand_u64_range` (was agent_loop.rs:695-755)
//! - `truncate_tool_result`, `compute_prompt_cache_key`, `default_max_autonomous_nudges` (was agent_loop.rs:493-550)
//! - `TOOL_RESULT_CHAR_LIMIT` constant (was agent_loop.rs:493-494)
//! - `is_interactive_tool` (P2.4, 2026-06-30) — names of tools that must run
//!   sequentially in the caller's task rather than the parallel `JoinSet`.
//! - `ToolErrorClass`, `classify_tool_error`, `ToolRecovery` (P2.5,
//!   2026-06-30) — classify a tool-call failure so the agent loop can pick
//!   the right recovery policy (re-prompt / swap tool / retry / backoff)
//!   without bespoke string matching at every error site.
//!
//! All public items are also re-exported from `agent_loop` (via
//! `pub use agent_helpers::...`) so existing callers like
//! `hermix_agent::agent_loop::ToolRegistry` keep working unchanged.

// ---------------------------------------------------------------------------
// Backwards-compat re-exports (AgentConfig was moved to agent_config.rs on
// 2026-06-30, REFACTOR_PLAN §4 / P2.2.4). Keeping a thin alias here so
// `hermix_agent::agent_helpers::AgentConfig` resolves to the same source.
// ---------------------------------------------------------------------------

pub use crate::agent_config::{AgentConfig, RetryConfig, RuntimeProviderConfig};

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use hermix_core::{ToolCall, ToolError, ToolSchema, UsageStats};

/// A single tool entry in the registry.
#[derive(Clone)]
pub struct ToolEntry {
    /// The tool's JSON Schema descriptor.
    pub schema: ToolSchema,
    /// A handler function: takes a JSON Value and returns the tool output string.
    pub handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("schema", &self.schema)
            .field("handler", &"<function>")
            .finish()
    }
}

/// A simple registry mapping tool names to their schemas and handlers.
///
/// The full-featured implementation lives in `hermes-tools`; this minimal
/// version exists so the agent loop can be tested and used independently.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        self.tools
            .insert(name.into(), ToolEntry { schema, handler });
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    /// Return all registered tool schemas.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|e| e.schema.clone()).collect()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Return a clone of this registry with the named tools
    /// removed. Used by [`crate::sub_agent_safety`] to give a
    /// `delegate_task` child a registry that physically
    /// cannot serve blocked tools, even if the LLM tries to
    /// call them.
    pub fn without_tools<I, S>(&self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = self.clone();
        for n in names {
            out.tools.remove(n.as_ref());
        }
        out
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool-result truncation
// ---------------------------------------------------------------------------

/// Maximum characters before we truncate a large tool result. ~1K tokens.
pub const TOOL_RESULT_CHAR_LIMIT: usize = 4000;

/// Truncate oversized tool results to avoid bloating the context window.
/// `read_file`/`terminal`/`web_fetch` are common offenders (large file contents, API responses).
/// Keeps the first `TOOL_RESULT_CHAR_LIMIT` chars and appends a notice with original size.
pub fn truncate_tool_result(content: &str, tool_name: Option<&str>) -> String {
    let total_len = content.len();
    if total_len <= TOOL_RESULT_CHAR_LIMIT {
        return content.to_string();
    }
    let is_large_tool = tool_name.is_some_and(|n| matches!(
        n,
        "read_file" | "read_pdf" | "terminal" | "exec_command"
            | "web_fetch" | "browser_snapshot" | "list_files" | "search_files"
    ));
    if !is_large_tool {
        return content.to_string();
    }
    let footer = format!(
        "\n\n[... 截断: {}/{} chars，可用 read_file 读取完整内容]",
        TOOL_RESULT_CHAR_LIMIT, total_len
    );
    let mut s = String::with_capacity(TOOL_RESULT_CHAR_LIMIT + footer.len());
    s.push_str(&content[..TOOL_RESULT_CHAR_LIMIT]);
    s.push_str(&footer);
    s
}

// ---------------------------------------------------------------------------
// Prompt-cache key + autonomous-nudge default
// ---------------------------------------------------------------------------

/// Compute a stable hash key for system prompt caching.
/// Only the config-driven factors that rarely change; excludes
/// per-turn data (tool list, user message, timestamp).
pub fn compute_prompt_cache_key(
    model: &str,
    personality: Option<&str>,
    system_prompt: Option<&str>,
    workspace: Option<&str>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::default();
    model.hash(&mut h);
    personality.unwrap_or("").hash(&mut h);
    system_prompt.unwrap_or("").hash(&mut h);
    workspace.unwrap_or("").hash(&mut h);
    h.finish()
}

/// How many times the autonomous loop may nudge the LLM back into action
/// (inject a "keep working" system message) before giving up and letting the
/// model's plain-text response through as a final answer.
///
/// Default is 8 — slightly higher than the old 5 because real-world browser
/// workflows (page navigation + captcha + form fill) commonly take 6–10
/// nudges before the LLM stops "talking about" the task and actually does it.
pub fn default_max_autonomous_nudges() -> u32 {
    8
}

// ---------------------------------------------------------------------------
// TurnMetrics
// ---------------------------------------------------------------------------

/// Timing and usage metrics for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnMetrics {
    /// Wall-clock time spent waiting for the LLM API, in milliseconds.
    pub api_time_ms: u64,
    /// Wall-clock time spent executing tools, in milliseconds.
    pub tool_time_ms: u64,
    /// Token usage for this turn (if reported by the provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

// ---------------------------------------------------------------------------
// Evolution counters (Python `_turns_since_memory` / `_iters_since_skill`)
// ---------------------------------------------------------------------------

/// Session-scoped counters for memory / skill nudges (mirrors Python `AIAgent` fields).
#[derive(Debug, Default)]
pub struct EvolutionCounters {
    pub turns_since_memory: u32,
    pub iters_since_skill: u32,
}

// ---------------------------------------------------------------------------
// ToolDispatchDecision / AgentCallbacks
// ---------------------------------------------------------------------------

/// Outcome of a pre-tool-dispatch hook.
///
/// Returned by `AgentCallbacks::on_pre_tool_dispatch` (when set) to control
/// whether (and how) a single tool call is executed. The hook fires once per
/// tool call, before arguments are parsed and the handler is invoked.
#[derive(Debug)]
pub enum ToolDispatchDecision {
    /// Proceed with normal execution.
    Allow,
    /// Skip execution and report the reason to the LLM as a tool error.
    Deny { reason: String },
    /// Block until the user (or upstream layer) resolves approval through the
    /// supplied `oneshot` receiver.
    ///
    /// The receiver yields `true` to allow execution, `false` to deny. If the
    /// sender is dropped before sending, execution is denied with a generic
    /// "approval channel closed" reason.
    AwaitApproval(oneshot::Receiver<bool>),
}

/// Callbacks invoked during tool execution for progress reporting.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Called when the LLM is "thinking" (reasoning tokens).
    pub on_thinking: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a tool call begins.
    ///
    /// Arguments: `tool_call_id` (a per-invocation uuid the agent loop
    /// generates and threads through `on_tool_complete` so callers can
    /// pair start/complete events), `tool` (function name), and
    /// `args` (the parsed arguments object).
    ///
    /// The `tool_call_id` is stable across one tool invocation —
    /// callers can stash it in a `HashMap` to correlate
    /// `on_tool_start` (before execution), any number of mid-flight
    /// progress events (future), and the matching `on_tool_complete`.
    pub on_tool_start: Option<Box<dyn Fn(&str, &str, &Value) + Send + Sync>>,
    /// Called when a tool call finishes.
    ///
    /// Same `tool_call_id` as the matching `on_tool_start`. `output`
    /// is the full result string returned by the tool handler.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str, &str) + Send + Sync>>,
    /// Called for each stream delta.
    pub on_stream_delta: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called after each completed LLM step (full response assembled).
    pub on_step_complete: Option<Box<dyn Fn(u32) + Send + Sync>>,
    /// Called when background memory/skill review completes or fails.
    ///
    /// Payload is a user-friendly summary string suitable for direct UI output.
    pub background_review_callback: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called for lifecycle/status notices (context pressure, retries, etc.).
    ///
    /// The first argument is a category string (e.g. `"lifecycle"`, `"activity"`).
    pub status_callback: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    /// Called before each tool dispatch to optionally gate execution.
    ///
    /// Returning `Allow` proceeds normally, `Deny` aborts the tool call with
    /// the given reason, and `AwaitApproval` blocks on a oneshot until the
    /// upstream layer (e.g. `LocalAgentService`) resolves the request.
    pub on_pre_tool_dispatch:
        Option<Box<dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync>>,
    /// H3: Called when the prefetcher resolves with an intent prediction.
    ///
    /// Payload is a JSON string containing `{ intent, confidence, results }`
    /// in the same shape as `StreamEvent::PrefetchIntent`.
    pub on_prefetch_intent: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// M5: Called when the agent emits an ACUI (Agent-Controlled UI)
    /// directive. The payload is a JSON-serialized `AcuiDirective`
    /// (tag = `"op"`, snake_case).
    ///
    /// Upstream layer (e.g. `LocalAgentService`) is expected to bridge
    /// this into a `StreamEvent::Acui` over the WebSocket so the
    /// frontend's `AcuiRenderer` can pick it up.
    pub on_acui_directive: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// 2026-06-29: Called whenever the context-compression pipeline
    /// transitions state (pressure detected, breaker tripped, recovery
    /// probe succeeded, etc.). The payload is a
    /// [`crate::compression::BreakerSnapshot`] borrowed for the
    /// duration of the call — the upstream layer is expected to
    /// serialise it (e.g. via `serde_json::to_value`) and forward as
    /// a `compaction_status` WS event. The `CompactionStatusBanner`
    /// UI hook in `apps/desktop-tauri/src/hooks/useStreamChat.ts`
    /// subscribes to that event and updates its own
    /// `compactionStatus` state.
    ///
    /// No-op if no `on_compaction_status` callback was wired (e.g.
    /// unit tests, batched runs that don't care about the UI side).
    /// The callback is invoked from the agent-loop task; the upstream
    /// is responsible for moving the data to whatever IO context
    /// owns the WebSocket for the active session.
    pub on_compaction_status:
        Option<Arc<dyn Fn(&crate::compression::BreakerSnapshot) + Send + Sync>>,
    /// 2026-06-29 (C3 marker protocol): Called from
    /// `resolve_initial_system_prompt` after the `SectionGate` audits
    /// the produced prompt and (when needed) auto-patches missing
    /// required sections. The payload is a borrowed
    /// [`crate::context::SectionGateReport`] — borrowed, not owned,
    /// so the callback MUST serialise it immediately rather than
    /// stash it on a long-lived queue.
    ///
    /// Upstream layer (e.g. `LocalAgentService`) bridges it into a
    /// `section_gate` WS event with shape
    /// `{ "event": "section_gate", "report": { patched: [...], pending: [...] } }`.
    /// Front-end `useStreamChat.ts` hook subscribes and forwards to
    /// the trace panel.
    ///
    /// No-op if no callback is wired (unit tests, batched runs).
    pub on_section_gate:
        Option<Arc<dyn Fn(&crate::context::SectionGateReport) + Send + Sync>>,
}

// ---------------------------------------------------------------------------
// Error classification + backoff
// ---------------------------------------------------------------------------

/// Classify an API error for retry/failover decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Retryable,
    RateLimit,
    ContextOverflow,
    Auth,
    Fatal,
}

pub fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many") {
        ErrorClass::RateLimit
    } else if lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("context_length_exceeded")
    {
        ErrorClass::ContextOverflow
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
    {
        ErrorClass::Auth
    } else if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("overloaded")
    {
        ErrorClass::Retryable
    } else {
        ErrorClass::Fatal
    }
}

/// Compute jittered exponential backoff delay.
pub fn jittered_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(max_ms);
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter / 2) + (rand_u64_range(0, jitter.max(1)));
    Duration::from_millis(delay)
}

pub fn rand_u64_range(min: u64, max: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::default();
    std::time::SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let h = hasher.finish();
    if max <= min {
        min
    } else {
        min + h % (max - min)
    }
}

// ---------------------------------------------------------------------------
// Interactive tools (P2.4, 2026-06-30)
// ---------------------------------------------------------------------------

/// Tools that must run **sequentially in the caller's task** rather than
/// in the parallel `JoinSet`.
///
/// Why interactive tools exist: a batch like
/// `[clarify, write_file, terminal_run]` would, under parallel dispatch,
/// kick off `write_file` and `terminal_run` *before* the user has answered
/// `clarify`. When the answer comes back, those results are now stale
/// context the LLM has to wade through. Running `clarify` first lets the
/// model pick better tool calls based on the user's reply.
///
/// `delegate_task` is already sequential by virtue of the
/// `SubAgentOrchestrator` path in `execute_tool_calls`; it is included
/// here so callers that ask "is this interactive?" get the full answer
/// without duplicating the orchestrator's logic.
pub fn is_interactive_tool(name: &str) -> bool {
    matches!(name, "clarify" | "delegate_task")
}

// ---------------------------------------------------------------------------
// ToolErrorClass (P2.5, 2026-06-30)
// ---------------------------------------------------------------------------

/// Classification of a tool-call failure for recovery-policy routing.
///
/// Each variant maps to a default recovery strategy the agent loop can
/// take without bespoke string-matching in `agent_loop.rs`:
/// - `ArgumentsCorrupted`     → re-prompt the LLM with the JSON parse
///                               error, do NOT count toward tool retry
///                               budget (LLM can usually self-correct).
/// - `ToolNotAvailable`       → send the LLM the available-tools list
///                               and let it pick a different tool.
/// - `ExecutionFailed`        → count toward retry budget; surface the
///                               error verbatim and let the LLM adapt.
/// - `Timeout`                → count toward retry budget; the LLM can
///                               pick a smaller-scope variant next time.
/// - `RateLimited`            → back off, retry same call after delay;
///                               do NOT surface to the LLM as a tool
///                               error (it isn't a logic problem).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorClass {
    ArgumentsCorrupted,
    ToolNotAvailable,
    ExecutionFailed,
    Timeout,
    RateLimited,
}

impl ToolErrorClass {
    /// Recommended recovery action for this class.
    pub fn recovery_action(self) -> ToolRecovery {
        match self {
            ToolErrorClass::ArgumentsCorrupted => ToolRecovery::Reprompt,
            ToolErrorClass::ToolNotAvailable => ToolRecovery::SwapTool,
            ToolErrorClass::ExecutionFailed => ToolRecovery::Retry,
            ToolErrorClass::Timeout => ToolRecovery::Retry,
            ToolErrorClass::RateLimited => ToolRecovery::Backoff,
        }
    }
}

/// Recovery action an agent loop should take for a classified tool error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRecovery {
    /// Re-feed the LLM with the parse error so it can re-issue valid JSON.
    Reprompt,
    /// Tell the LLM which tools *are* available so it can pick another.
    SwapTool,
    /// Retry the same call (count toward `invalid_tool_call_max_retries`).
    Retry,
    /// Back off for a `RateLimit`-style duration before retrying.
    Backoff,
}

/// Classify a tool-call error message into a [`ToolErrorClass`].
///
/// The classification is heuristic — it inspects the human-readable
/// error string the agent loop currently writes into `ToolResult::err`
/// and into the `tool_errors` log. Patterns are chosen to match the
/// actual error messages produced by:
///   - `serde_json::from_str` failures (ArgumentsCorrupted)
///   - `Unknown tool 'X'` from the registry (ToolNotAvailable)
///   - handlers' `Err(e.to_string())` (ExecutionFailed)
///   - per-tool timeouts (Timeout)
///   - HTTP 429 / "rate limit" from providers (RateLimited)
pub fn classify_tool_error(err: &str) -> ToolErrorClass {
    let lower = err.to_lowercase();
    if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many requests")
    {
        ToolErrorClass::RateLimited
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline exceeded")
    {
        ToolErrorClass::Timeout
    } else if lower.starts_with("unknown tool") || lower.contains("not registered") {
        ToolErrorClass::ToolNotAvailable
    } else if lower.starts_with("invalid json params")
        || lower.contains("expected value")
        || lower.contains("expected `")
        || lower.contains("trailing characters")
    {
        ToolErrorClass::ArgumentsCorrupted
    } else {
        ToolErrorClass::ExecutionFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_set_includes_clarify_and_delegate() {
        assert!(is_interactive_tool("clarify"));
        assert!(is_interactive_tool("delegate_task"));
    }

    #[test]
    fn batched_tools_are_not_interactive() {
        assert!(!is_interactive_tool("write_file"));
        assert!(!is_interactive_tool("terminal"));
        assert!(!is_interactive_tool("read_file"));
        assert!(!is_interactive_tool("list_files"));
        assert!(!is_interactive_tool(""));
    }

    #[test]
    fn classify_tool_error_recognises_rate_limit() {
        assert_eq!(
            classify_tool_error("HTTP 429 Too Many Requests"),
            ToolErrorClass::RateLimited
        );
        assert_eq!(
            classify_tool_error("upstream rate limit hit"),
            ToolErrorClass::RateLimited
        );
    }

    #[test]
    fn classify_tool_error_recognises_timeout() {
        assert_eq!(
            classify_tool_error("connection timed out after 30s"),
            ToolErrorClass::Timeout
        );
        assert_eq!(
            classify_tool_error("deadline exceeded"),
            ToolErrorClass::Timeout
        );
    }

    #[test]
    fn classify_tool_error_recognises_unknown_tool() {
        assert_eq!(
            classify_tool_error("Unknown tool 'foo'. Available tools: [bar, baz]"),
            ToolErrorClass::ToolNotAvailable
        );
    }

    #[test]
    fn classify_tool_error_recognises_bad_json() {
        assert_eq!(
            classify_tool_error(
                "Invalid JSON params for tool 'write_file': expected value at line 1"
            ),
            ToolErrorClass::ArgumentsCorrupted
        );
        assert_eq!(
            classify_tool_error("trailing characters at line 5"),
            ToolErrorClass::ArgumentsCorrupted
        );
    }

    #[test]
    fn classify_tool_error_defaults_to_execution_failed() {
        assert_eq!(
            classify_tool_error("io error: file not found"),
            ToolErrorClass::ExecutionFailed
        );
        assert_eq!(
            classify_tool_error("permission denied"),
            ToolErrorClass::ExecutionFailed
        );
    }

    #[test]
    fn recovery_action_matches_class() {
        assert_eq!(ToolErrorClass::ArgumentsCorrupted.recovery_action(), ToolRecovery::Reprompt);
        assert_eq!(ToolErrorClass::ToolNotAvailable.recovery_action(), ToolRecovery::SwapTool);
        assert_eq!(ToolErrorClass::ExecutionFailed.recovery_action(), ToolRecovery::Retry);
        assert_eq!(ToolErrorClass::Timeout.recovery_action(), ToolRecovery::Retry);
        assert_eq!(ToolErrorClass::RateLimited.recovery_action(), ToolRecovery::Backoff);
    }
}