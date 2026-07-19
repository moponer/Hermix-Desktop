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
//! - `compute_prompt_cache_key`, `default_max_autonomous_nudges` (was agent_loop.rs:493-550)
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
use tokio::sync::{oneshot, Notify};

use hermix_core::{ApprovalOutcome, ToolCall, ToolError, ToolSchema, UsageStats};

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

    /// Return a clone of this registry with only the named tools
    /// retained. Used by [`crate::sub_agent_orchestrator`] to give
    /// role-restricted sub-agents a registry that only contains
    /// the tools allowed for their role (P6-4).
    pub fn retain_only<F>(&self, predicate: F) -> Self
    where
        F: Fn(&str) -> bool,
    {
        let mut out = self.clone();
        out.tools.retain(|name, _| predicate(name));
        out
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool-result truncation (REMOVED — see budget::truncate_result)
// ---------------------------------------------------------------------------
//
// Previously this module owned `truncate_tool_result` + `TOOL_RESULT_CHAR_LIMIT`,
// which byte-sliced `content[..4000]` (panic on CJK mid-character) and applied
// a hard 4 000-char cap on a hardcoded list of "large tools". Call sites in
// `agent_loop.rs` then double-truncated: `budget::enforce_budget` (char-safe
// after A1) ran first, then `truncate_tool_result` re-cut to 4 000 chars.
//
// Now there is one truncation path: `budget::truncate_result` (char-safe)
// invoked from `budget::enforce_budget`. The per-tool allowlist and the
// "可用 read_file 读取完整内容" footer were a UX nicety — the budget's
// "[... truncated N characters ...]" footer carries the same information.
// Configure `BudgetConfig::max_result_size_chars` per-call-site if a
// particular tool needs a tighter cap than the global default.

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

/// Compute a context-layer cache key.
///
/// The context layer covers memory / skills / context-files blocks — it
/// changes when the underlying memory store mutates (new facts, pruned
/// entries), when skills get added/removed, or when a workspace context
/// file is rewritten. None of those are per-turn data, so they are still
/// safe to cache, but they invalidate independently of the stable layer
/// (model / personality / system_prompt / workspace config).
///
/// Keyed only by a coarse `epoch` tick the caller passes in — callers
/// bump the epoch whenever they mutate the underlying stores; this keeps
/// the key short and avoids dragging large text into the hasher.
pub fn compute_context_cache_key(epoch: u64) -> u64 {
    // Just return the epoch directly. It's already a u64 cache key;
    // folding it through a hasher would buy us nothing.
    epoch
}

/// Three-tier prompt cache slot.
///
/// Layered to keep the largest piece of the system prompt (the stable
/// identity / directions / tool block) reusable across turns even when
/// the smaller context block mutates, and vice versa. The volatile
/// block (timestamp + session_id) is rebuilt every turn and is not
/// cached.
///
/// Per-process cache slot for the two long-lived layers of the
/// system prompt. Each layer carries its own `cached_*_prompt_key`
/// so that:
///  - the cache invalidates immediately when the upstream key
///    changes (e.g. config edit, model swap, memory epoch tick),
///  - external observers (telemetry, the dashboard cache-warming
///    panel, CI smoke tests) can read the key back out via the
///    `last_*_key()` getters without taking a lock.
///
/// `None` for the text slot means "miss — re-render the layer".
/// `None` for the key is treated as a miss for the same reason,
/// so the upstream key never diverges from what was actually
/// stored.
#[derive(Default)]
pub struct PromptCache {
    /// Most recent stable-layer render key, or `None` if the
    /// stable layer has never been rendered in this process.
    pub cached_stable_prompt_key: Option<u64>,
    /// Rendered text for the stable layer at `cached_stable_prompt_key`.
    pub stable: Option<String>,
    /// Most recent context-layer render key, or `None` if the
    /// context layer has never been rendered.
    pub cached_context_prompt_key: Option<u64>,
    /// Rendered text for the context layer at `cached_context_prompt_key`.
    pub context: Option<String>,
}

impl PromptCache {
    /// Read both layers' stored text. Returns `None` for a layer
    /// when the slot is empty or its key does not match the supplied
    /// expected key for that layer.
    pub fn lookup(
        &self,
        expected_stable: u64,
        expected_context: u64,
    ) -> (Option<&str>, Option<&str>) {
        let stable = match (self.cached_stable_prompt_key, self.stable.as_ref()) {
            (Some(k), Some(text)) if k == expected_stable && !text.is_empty() => {
                Some(text.as_str())
            }
            _ => None,
        };
        let context = match (self.cached_context_prompt_key, self.context.as_ref()) {
            (Some(k), Some(text)) if k == expected_context && !text.is_empty() => {
                Some(text.as_str())
            }
            _ => None,
        };
        (stable, context)
    }

    /// Store the freshly-rendered stable layer text + the key that
    /// produced it. Both fields are written together; reading either
    /// one without the other (theoretically possible from an
    /// external observer taking a snapshot mid-update) is safe
    /// because the lookup always cross-checks both.
    pub fn store_stable(&mut self, key: u64, text: String) {
        self.cached_stable_prompt_key = Some(key);
        self.stable = Some(text);
    }

    /// Store the freshly-rendered context layer text + key. See
    /// `store_stable` for atomicity semantics.
    pub fn store_context(&mut self, key: u64, text: String) {
        self.cached_context_prompt_key = Some(key);
        self.context = Some(text);
    }

    /// Return the most-recent stable layer cache key, or `None`
    /// if the stable layer has never been rendered in this process.
    /// Read-only; takes no lock when called through `&self` from
    /// inside a held `MutexGuard`.
    pub fn last_stable_key(&self) -> Option<u64> {
        self.cached_stable_prompt_key
    }

    /// Return the most-recent context layer cache key, or `None`.
    /// See `last_stable_key` for notes.
    pub fn last_context_key(&self) -> Option<u64> {
        self.cached_context_prompt_key
    }

    /// Return the current cache state as a snapshot suitable for
    /// telemetry: `(stable_key, stable_len, context_key, context_len)`.
    /// Used by `build_system_prompt` to emit the
    /// `prompt_cache_state` tracing event so external observers
    /// can correlate cache hits with key changes without holding
    /// the lock.
    pub fn snapshot(&self) -> (Option<u64>, usize, Option<u64>, usize) {
        (
            self.cached_stable_prompt_key,
            self.stable.as_ref().map(|s| s.len()).unwrap_or(0),
            self.cached_context_prompt_key,
            self.context.as_ref().map(|s| s.len()).unwrap_or(0),
        )
    }
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
    /// The receiver yields an [`ApprovalOutcome`]: `Approved` allows
    /// execution, while `DeniedByUser` / `TimedOut { .. }` /
    /// `ChannelClosed` deny it. The richer enum (vs. the old `bool`)
    /// lets `tool_dispatch::resolve_pre_dispatch` build an accurate
    /// `tool_result` reason and trigger the structured denial system
    /// note (see `tool_result_processor::push_tool_result`).
    AwaitApproval(oneshot::Receiver<ApprovalOutcome>),
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
    /// `denial_kind` is the `ToolResult::denial_kind` for this call —
    /// `""` for a normal success, `"policy"` for a hardline gate, and
    /// `"user_denied"` / `"timed_out"` / `"channel_closed"` for a
    /// user-facing denial the frontend should surface as a banner.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str, &str, &str) + Send + Sync>>,
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
    /// 2026-07-19 (type-B root-cause): forwarded from
    /// `AgentOverrides::shutdown` by `LocalAgentService`. When the
    /// server marks the WebSocket as disconnected, it fires
    /// `notify_one()` on this `Arc<Notify>`, and the pre-tool
    /// dispatch path races it against the approval oneshot so the
    /// agent loop never dies mid-`rx.await` with a half-initialized
    /// context. See `traits::AgentOverrides::shutdown` for the
    /// cross-crate contract.
    pub shutdown: Option<Arc<Notify>>,
    /// M5: Called when the agent emits an ACUI (Agent-Controlled UI)
    /// directive. The payload is a JSON-serialized `AcuiDirective`
    /// (tag = `"op"`, snake_case).
    ///
    /// Upstream layer (e.g. `LocalAgentService`) is expected to bridge
    /// this into a `StreamEvent::Acui` over the WebSocket so the
    /// frontend's `AcuiRenderer` can pick it up.
    pub on_acui_directive: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// O7.2: Called when the `clarify` tool fires — the agent is
    /// asking the user a question mid-turn. `question` is the text
    /// the agent wants the user to answer, rendered verbatim from
    /// the tool arguments. The frontend can use this to show an
    /// inline input prompt instead of a generic tool card.
    pub on_clarify: Option<Box<dyn Fn(&str) + Send + Sync>>,
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
    /// P3-2: Called when the agent loop enters a new phase.
    ///
    /// Payload is a JSON string: `{"phase":"llm_call","stage":"reasoning","turn":3}`.
    /// The upstream layer (e.g. `LocalAgentService`) bridges this into a
    /// `LoopPhaseChanged` WS event via the orchestration bus.
    ///
    /// No-op if no callback is wired (unit tests, batched runs).
    pub on_loop_phase_changed: Option<Arc<dyn Fn(&str, &str, u32) + Send + Sync>>,
    /// P3-5: Called when the agent loop finalizes, indicating the task
    /// should be marked as completed or failed.
    ///
    /// Arguments: `(success: bool, task_id: Option<&str>)`.
    /// The upstream layer uses this to transition the Task status and
    /// trigger result delivery (Phase 7).
    ///
    /// No-op if no callback is wired.
    pub on_task_complete:
        Option<Arc<dyn Fn(bool, Option<&str>) + Send + Sync>>,
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

    // Rate limiting — exact status or well-known phrases
    if lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
        || lower.contains("quota exceeded")
    {
        return ErrorClass::RateLimit;
    }

    // Context overflow
    if lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("context_length_exceeded")
        || lower.contains("reduce the length")
    {
        return ErrorClass::ContextOverflow;
    }

    // Auth — exact status codes and well-known phrases
    // Use word-boundary checks for status codes to avoid substring matches
    if lower.contains("401") && !lower.contains("4010") && !lower.contains("4011")
        || lower.contains("403") && !lower.contains("4030") && !lower.contains("4031")
        || lower.contains("unauthorized")
        || lower.contains("unauthenticated")
        || lower.contains("authentication failed")
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("access denied")
        || lower.contains("forbidden")
    {
        return ErrorClass::Auth;
    }

    // Retryable server / network errors
    if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("eof")
        || lower.contains("broken pipe")
        || lower.contains("tls")
        || lower.contains("dns")
        || lower.contains("overloaded")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
    {
        return ErrorClass::Retryable;
    }

    // Default: fatal
    ErrorClass::Fatal
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