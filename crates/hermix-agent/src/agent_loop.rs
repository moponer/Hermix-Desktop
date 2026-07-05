//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermix_focus::{FocusStack, SessionFrame, SessionFrameStatus};
#[cfg(test)]
use hermix_focus::MemoryLayer;
// `chrono` (DateTime/Utc) dropped on 2026-07-04 when `OAuthStoreCredential`
// moved to `crate::oauth_refresh`.
use futures::StreamExt;
// serde::Serialize / serde::Deserialize were dropped on 2026-07-04 when
// `OAuthStoreCredential` moved to `crate::oauth_refresh`; this file no
// longer holds any #[derive(Serialize, Deserialize)] structs.
use serde_json::{json, Value};
use tokio::task::JoinSet;
use tokio::time::sleep;

use hermix_core::{
    format_token_count, AcuiDirective, AgentError, AgentResult, CompressionEngine, LlmProvider, LlmResponse, Message,
    MessageRole, StreamChunk, ToolCall, ToolCallSanitizer, ToolResult, ToolSchema,
    UsageStats,
};
use hermix_intelligence::{
    prefetch::Prefetcher,
    AlwaysInclude, KeywordRouter, ToolSchemaRouter, TurnContext,
};

use crate::archive::{archive_context_section, render_archive_summary, ArchiveRef};
use crate::budget;
use crate::context::ContextManager;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::plugins::{HookResult, HookType, PluginManager};
use crate::python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, ThinkBlockParser,
};
use crate::smart_model_routing::TurnRouteSignature;
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
use tracing::{info, instrument};

// ---------------------------------------------------------------------------
// Backwards-compat re-exports (P2.2.4, 2026-06-30)
// ---------------------------------------------------------------------------
//
// `AgentConfig`, `RetryConfig`, and `RuntimeProviderConfig` moved to
// `crate::agent_config` on 2026-06-30 (REFACTOR_PLAN §4). Two test files
// still import via `hermix_agent::agent_loop::AgentConfig`, so we keep
// these `pub use` aliases live — same source-of-truth, just a thinner
// alias to the new module.
//
// `ToolEntry`, `ToolRegistry`, `TurnMetrics`, `EvolutionCounters`,
// `ToolDispatchDecision`, `AgentCallbacks`, `ErrorClass`, the helper
// functions, and `TOOL_RESULT_CHAR_LIMIT` were extracted to
// `crate::agent_helpers` on 2026-06-30 (P2.2.5). Same source-of-truth,
// re-exported below so existing callers (e.g.
// `hermix_agent::agent_loop::ToolRegistry` in hermix-cron and the test
// files) keep working unchanged.

pub use crate::agent_config::{AgentConfig, RetryConfig, RuntimeProviderConfig};
pub use crate::agent_helpers::{
    classify_error, classify_tool_error, compute_prompt_cache_key, default_max_autonomous_nudges,
    jittered_backoff, rand_u64_range, AgentCallbacks,
    ErrorClass, EvolutionCounters, ToolDispatchDecision, ToolEntry, ToolErrorClass, ToolRecovery,
    ToolRegistry, TurnMetrics,
};

// ---------------------------------------------------------------------------
// OAuth credential + refresh manager (extracted 2026-07-04)
// ---------------------------------------------------------------------------
//
// `OAuthRefresh` + `OAuthStoreCredential` moved to `crate::oauth_refresh`.
// Kept as `pub use` aliases so `hermix_agent::agent_loop::OAuthRefresh`
// remains a valid import path for any external consumer (e.g. hermix-cron,
// test files). The implementation now lives next to the HTTP code it
// orchestrates (reqwest POST, on-disk JSON store, per-provider backoff)
// — see `oauth_refresh.rs` for the full surface and unit tests.

pub use crate::oauth_refresh::{OAuthRefresh, OAuthStoreCredential};

// Python `AIAgent._MEMORY_REVIEW_PROMPT` / `_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT` (v2026.4.16)
const MEMORY_REVIEW_PROMPT: &str = "Review the conversation above and consider saving to memory if appropriate.\n\n\
Focus on:\n\
1. Has the user revealed things about themselves — their persona, desires, preferences, or personal details worth remembering?\n\
2. Has the user expressed expectations about how you should behave, their work style, or ways they want you to operate?\n\n\
If something stands out, save it using the memory tool. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const SKILL_REVIEW_PROMPT: &str =
    "Review the conversation above and consider saving or updating a skill if appropriate.\n\n\
Focus on: was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome?\n\n\
**P0-1 (auto-skill learning loop)**: when you decide a skill is worth saving, \
prefer `skill_manage(action='auto_extract_from_conversation')` over `auto_create`. \
It enforces a quality gate (name 3–80 chars, summary >10 chars and non-placeholder, \
≥1 step) and writes SKILL.md frontmatter so the orchestrator picks the skill up \
cleanly. It also dedupes — if a skill with the same name exists, it updates instead \
of creating a duplicate.\n\n\
Provide a `name` (kebab-case slug, 3–80 chars), a `summary` (one paragraph, >10 \
chars), an ordered `steps` array (each step a concrete actionable instruction), \
and optionally a `rationale` explaining why this is worth remembering next time. \
If a relevant skill with the same name already exists, the dedup logic will route \
your call to an update automatically — just call `auto_extract_from_conversation`.\n\n\
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const COMBINED_REVIEW_PROMPT: &str = "Review the conversation above and consider two things:\n\n\
**Memory**: Has the user revealed things about themselves — their persona, \
desires, preferences, or personal details? Has the user expressed expectations \
about how you should behave, their work style, or ways they want you to operate? \
If so, save using the memory tool.\n\n\
**Skills**: Was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome? **Prefer \
`skill_manage(action='auto_extract_from_conversation')`** — it enforces a quality \
gate, writes SKILL.md frontmatter, and dedupes against existing skills by name. \
Pass `name` (3–80 chars), `summary` (>10 chars), `steps` (≥1), and optionally \
`rationale`.\n\n\
Only act if there's something genuinely worth saving. \
If nothing stands out, just say 'Nothing to save.' and stop.";

use crate::stream_collect::StreamCollectOutcome;

// ---------------------------------------------------------------------------
/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
    /// Primary LLM credential pool (Python `primary["credential_pool"]` / runtime pool).
    pub primary_credential_pool: Option<Arc<CredentialPool>>,
    /// Memory/skill nudge counters (persist for the lifetime of this `AgentLoop`).
    pub evolution_counters: Arc<Mutex<EvolutionCounters>>,
    /// Manager for runtime-provider OAuth token refresh + backoff state.
    pub oauth_refresh: OAuthRefresh,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    ///
    /// `pub(crate)` so `tool_executor.rs` can read it from the moved
    /// `execute_tool_calls` method (2026-07-04). The field stays
    /// crate-private — external callers must use the agent builder API.
    pub(crate) sub_agent_orchestrator: Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Mid-run `/steer` buffer (Python `AIAgent._pending_steer`).
    pending_steer: Arc<Mutex<Option<String>>>,
    /// Optional tool schema router. When set, the LLM only sees the routed
    /// subset of `tool_registry` (Phase 7 H1 — token saving, see
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3). When `None`, the full
    /// registry is sent (legacy behavior).
    pub tool_router: Option<Arc<dyn ToolSchemaRouter>>,
    /// Phase 15: Time-aware recall. When set, `memory_prefetch`
    /// extracts a `time_window` from the user query, fans out to all
    /// `MemoryLayer` backends through the router, deduplicates and
    /// re-ranks hits by salience, and injects the resulting structured
    /// blocks as a `## Memory` section of the system prompt. This is
    /// the **only** read path; the legacy `MemoryManager::prefetch_all`
    /// markdown path was removed in Phase 16.
    ///
    /// Phase 17: the **write** path is also wired through this
    /// struct's sibling `plugin_manager: PluginManager`. The seven
    /// write-path helpers (`memory_sync`, `notify_memory_writes`,
    /// `notify_delegations`, `memory_on_turn_start`,
    /// `memory_system_prompt`, `memory_pre_compress_note`,
    /// `memory_on_session_end`) iterate
    /// `MemoryRouter` to fan out to each
    /// bundled `MemoryProviderPlugin`.
    pub memory_router: Option<Arc<hermix_focus::MemoryRouter>>,
    /// Always-include whitelist (read_file/write_file/memory by default).
    pub always_include: Vec<String>,
    /// Phase 15 H2: Intent-aware prefetcher. When set, the agent loop
    /// classifies the user message into a `PrefetchIntent` *before*
    /// the LLM call, dispatches read-only (Safe) tools in parallel,
    /// and injects results as a system-prompt block. This reduces
    /// the time-to-first-token by overlapping tool execution with
    /// LLM reasoning.
    pub prefetcher: Option<Arc<Prefetcher>>,
    /// Learned prefetch stats (persisted across turns/sessions).
    learned_prefetch_stats: Arc<Mutex<hermix_intelligence::prefetch::IntentStats>>,
    /// M1: per-session `FocusStack<SessionFrame>`. The agent loop
    /// pushes a frame on every `delegate_task` invocation and pops
    /// it on completion; a `FocusConclusion` is auto-generated and
    /// fed to the memory router for long-term indexing. See
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 + M1.
    ///
    /// The stack starts empty; callers push the session root
    /// (one frame per session) at `run()` start via
    /// `note_session_started`, and sub-agents push/pop via
    /// `note_delegation_start` / `note_delegation_end`.
    pub focus_stack: Arc<Mutex<FocusStack<SessionFrame>>>,
    /// M1.6.3: optional consciousness tracker for broadcasting
    /// `focus_depth` changes when the focus stack pushes/pops.
    /// Wired by `LocalAgentService::spawn_agent_loop` so the
    /// "ECG" stream the desktop renders reflects nested
    /// delegation depth in real time.
    consciousness_tracker: Option<Arc<crate::consciousness::ConsciousnessTracker>>,
    /// Advanced context compressor (LLM-powered summarization).
    /// When set, replaces the simple character-based compressor in
    /// `auto_compress_if_over_threshold`. Requires an `AuxiliaryClient`
    /// for summary generation — constructed from env vars in
    /// `LocalAgentService::spawn_agent_loop`.
    compression: Option<Arc<tokio::sync::Mutex<dyn CompressionEngine + Send>>>,
    /// Latest compressed context summary from the most recent compaction.
    /// Stored here so it can be included in AgentResult for persistence.
    latest_compressed_summary: std::sync::Mutex<Option<String>>,
    /// 2026-06-26 (A): raw `turns_to_summarize` from the most recent
    /// compression, captured by `ContextCompressor::compress` before
    /// it mutated the conversation. `local_agent_service` reads this
    /// in the same `persist_session_with_summary` transaction that
    /// writes `sessions.compressed_summary`, and forwards it to
    /// `SessionPersistence::archive_compressed_turns` so the
    /// original wording is durable in `compression_archives`.
    latest_compression_archived_turns: std::sync::Mutex<Option<Vec<Message>>>,
    /// 2026-06-26 (B): metrics from the most recent compression
    /// (original/compressed turns + tokens + savings). Forwarded
    /// alongside the archived turns so `save_session_atomic` can
    /// write `compression_summary_history` in the same transaction.
    latest_compression_metrics: std::sync::Mutex<Option<crate::compression::CompressionMetrics>>,
    /// Most recent archive refs produced by
    /// [`archive_context_section`]. Surfaced to the frontend so the
    /// user can open the archive file at a specific node anchor.
    /// See `crates/hermix-agent/src/archive.rs` (v2: 2026-06-21).
    last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>>,
    /// Phase 18 (memory recall gating): `false` until the first full
    /// memory recall completes. Once `true`, subsequent `memory_prefetch`
    /// calls use a smaller limit to avoid re-injecting the same memory
    /// every turn — the LLM already has it in the active context window.
    memory_recall_seeded: std::sync::atomic::AtomicBool,
    /// Phase 18 (prompt caching): three-tier prompt cache. Holds the
    /// rendered stable layer (identity, directions, tool block,
    /// personality, workspace, provider hint) and context layer
    /// (memory / skills / context files), each independently keyed so
    /// a context-store mutation does not force the larger stable layer
    /// to re-render. The volatile layer (timestamp + session_id) is
    /// rebuilt every turn and not cached.
    prompt_cache: std::sync::Mutex<crate::agent_helpers::PromptCache>,
    /// Monotonic epoch bumped when memory/skills/context-files stores
    /// mutate; forms the key for the context cache layer.
    context_epoch: std::sync::atomic::AtomicU64,
    /// Optional embedding provider for vector recall in memory prefetch.
    /// When set, `memory_prefetch` generates embeddings and passes them
    /// to `MemoryQuery` for cosine similarity search.
    embedding_provider: Option<Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>>,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnRuntimeRoute {
    pub(crate) model: String,
    pub(crate) provider: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key_env: Option<String>,
    pub(crate) api_mode: Option<ApiMode>,
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) credential_pool: Option<Arc<CredentialPool>>,
    /// When true (default), merge with [`AgentLoop::primary_credential_pool`] if route pool is unset.
    pub(crate) credential_pool_fallback: bool,
    pub(crate) route_label: Option<String>,
    pub(crate) routing_reason: Option<String>,
    pub(crate) signature: TurnRouteSignature,
}

/// Output of [`AgentLoop::prepare_run_context`]: everything needed to enter
/// the model turn loop without re-running the setup phase.
pub(crate) struct PreparedRun {
    pub ctx: ContextManager,
    /// Tools the turn loop will pass to the model on each call.
    pub tool_schemas: Vec<ToolSchema>,
    /// Whether OnSessionStart hooks already fired during setup. If true, the
    /// turn loop must NOT re-fire them (otherwise: double-fire).
    pub session_started_hooks_fired: bool,
    /// Index of the last user message in `ctx`, if `persist_user_message` is
    /// configured. Used at the end to write it back to the session store.
    pub persist_user_idx: Option<usize>,
    /// Counter for Codex-style "ack then continue" handling across turns.
    pub codex_ack_continuations: u32,
    /// Counter for autonomous mode nudges (non-streaming path).
    pub autonomous_nudge_count: u32,
    /// Whether the agent should review memory at the end of this run (set if
    /// `memory_nudge_interval` reached its threshold during setup).
    pub review_memory_at_end: bool,
    /// Text of the most-recent user message (the original task). Used by
    /// Codex-style "looks_like_codex_intermediate_ack" and memory review.
    pub task_hint: String,
}

/// Encapsulates the 3 tracking fields the context-pressure warning
/// logic uses. Lifted out of [`LoopState`] so the cooldown/throttle
/// rules live in one place (and can be unit-tested without spinning
/// up an `AgentLoop`).
///
/// Phase 5 (2026-07-03): replaces 3 fields (`context_pressure_warned_at`,
/// `context_pressure_last_warn_at`, `context_pressure_last_warn_percent`)
/// scattered across `LoopState`. The shared [`should_emit_warning`]
/// method now owns the cooldown + tier-upgrade + percent-step rules.
///
/// Physically split to `context_pressure.rs` (2026-07-04); re-exported
/// here so existing `state.context_pressure.*` call paths compile unchanged.
pub use crate::context_pressure::ContextPressureMonitor;

/// Phase 5 (2026-07-03): cost-guard bookkeeping consolidated into one
/// struct. The four sibling fields (`accumulated_usage`,
/// `session_cost_usd`, `cost_warned`, `forced_runtime_route`) used to
/// live directly on `LoopState` and were passed one-by-one as 4 sibling
/// `&mut` arguments into [`AgentLoop::process_post_response`]. Folding
/// them into one struct:
/// - shrinks `LoopState` from 14 to 10 fields,
/// - shrinks `process_post_response` from 7 to 3 mutable args (3 scalars
///   stay as primitives; the bookkeeping is now `&mut CostGuard`),
/// - keeps the 4-field atomic-mutation at the cost-guard code path
///   where it belongs (a single helper can read + write the state),
/// - lets `publish_from` drop another 4 args.
///
/// The 4 fields stay co-located on `self.cost_guard` and mutate in
/// place via the `record_usage` / `apply_route_downgrade` methods.
/// Reset / `Default` produce the same all-zeros state as before, so
/// `LoopState::default()` semantics are unchanged.
///
/// Physically split to `cost_guard.rs` (2026-07-04); re-exported here
/// so existing `state.cost_guard.*` call paths compile unchanged.
/// `TurnRuntimeRoute` was lifted to `pub(crate)` so the new module
/// can name it; the field visibility stays `pub(crate)` so the
/// runtime publish path (`state.cost_guard.accumulated_usage`) keeps
/// working without accessor-method churn.
pub use crate::cost_guard::{CostGuard, CostGuardAction};
pub(crate) use crate::cost_guard::WarnKind;
pub(crate) use crate::cost_routing::RouteState;

/// Phase 5 (2026-07-03): per-loop retry counters consolidated into one
/// struct so `LoopState` doesn't have to carry 3 sibling `u32` fields and
/// `publish_from` doesn't have to take 3 sibling `u32` arguments.
///
/// All three counters follow the same shape:
/// - `bump_*` saturating-increments and returns the new value.
/// - `reset_*` clears the counter (called on the success path).
/// - The `< max` / `>= max` comparison stays at the call site because
///   the action on max differs between the three (finalize vs.
///   continue-with-error vs. fall-through) — folding that decision
///   into the helper would force one path on all three.
///
/// Physically split to `retry_counters.rs` (2026-07-04); re-exported
/// here so existing `state.retry_counters.*` call paths compile
/// unchanged.
pub use crate::retry_counters::RetryCounters;

/// Phase 5 (2026-07-04 follow-up): per-run LLM-result caches consolidated
/// into one struct. The two sibling `Option` fields
/// (`last_checkpoint_messages`, `last_content_with_tools`) used to live
/// directly on `LoopState` and were bridged back from `run()`'s
/// loop-locals via 2 sibling arguments in `publish_from`. Folding them
/// into one struct:
/// - shrinks `LoopState` by 2 fields (8 → 6),
/// - shrinks `publish_from` from 3 args to 1 arg (only `total_turns`
///   remains bridged),
/// - lets `run()` mutate the caches in place via
///   `store_checkpoint` / `store_content_with_tools` /
///   `take_content_with_tools` instead of accumulating to a local
///   alias and re-publishing each iteration,
/// - collapses 2 helper-arg slots (`&mut Option<String>` for
///   `last_content_with_tools`) to 1 (`&mut MessageCache`).
///
/// The 2 fields stay co-located on `self.message_cache` and mutate
/// in place. Reset / `Default` produce the same all-`None` state as
/// before, so `LoopState::default()` semantics are unchanged.
///
/// Physically split to `message_cache.rs` (2026-07-04); re-exported
/// here so existing `state.message_cache.*` call paths compile
/// unchanged.
pub use crate::message_cache::MessageCache;

/// Phase 5 (2026-07-04 follow-up): turn-level timing bookkeeping consolidated
/// into one struct. The two sibling `u64` fields (`total_api_time_ms`,
/// `total_tool_time_ms`) used to live directly on `LoopState` and were
/// bridged back from `run()`'s loop-locals via 2 sibling arguments in
/// `publish_from`. Folding them into one struct:
/// - shrinks `LoopState` by 2 fields (10 → 8),
/// - shrinks `publish_from` from 5 args to 3 args,
/// - lets `run()` mutate the timings in place via `record_api` /
///   `record_tool` instead of accumulating to a local `_total_*` and
///   re-publishing each iteration (the local accumulation was the
///   pre-extraction pattern that the Phase 1.1 bridge set up for
///   `total_turns`; timings don't need that bridge because nothing
///   reads `state.timings` from outside this file — they're just
///   collected for future inspection / metrics).
///
/// The 2 fields stay co-located on `self.timings` and mutate in
/// place. Reset / `Default` produce the same all-zeros state as
/// before, so `LoopState::default()` semantics are unchanged.
///
/// Physically split to `turn_timings.rs` (2026-07-04); re-exported
/// here so existing `state.timings.*` call paths compile unchanged.
pub use crate::turn_timings::TurnTimings;

/// All mutable loop-local state shared between `run()` and `run_stream()`.
/// Centralized to avoid declaring 17+ individual locals in both methods
/// (reduces duplication and eliminates the "forget to sync" bug).
///
/// Phase 1 (2026-07-03) introduces the struct. Phase 1.1 wires the
/// 14 run() + 17 run_stream() locals through it via `publish_from`.
/// Phase 5 (2026-07-03 + 2026-07-04) folds 9 inline fields into 4
/// manager structs ([`TurnTimings`], [`CostGuard`], [`MessageCache`],
/// [`RetryCounters`], [`ContextPressureMonitor`]). After Phase 5
/// (commits a37bba3, d31f7e8) the `publish_from` bridge is gone —
/// `run()` / `run_stream()` write `state.total_turns = total_turns;`
/// directly; the inner manager structs are mutated in place via their
/// own `record_*` / `bump_*` / `store_*` methods.
#[derive(Default)]
pub(crate) struct LoopState {
    /// Total turns completed so far in this run.
    pub(crate) total_turns: u32,
    /// Per-run API + tool wall-clock accumulators. Phase 5 (2026-07-04):
    /// extracted from 2 inline `u64` fields to a [`TurnTimings`] struct
    /// so `publish_from` shrinks by 2 args and the `record_api` /
    /// `record_tool` methods are the single mutation path.
    pub(crate) timings: TurnTimings,
    /// Cost-guard bookkeeping: cumulative usage, USD spend, warn flag.
    /// Phase 5 (2026-07-03): 4 inline fields that used to sit directly
    /// on `LoopState` moved into [`CostGuard`] which is mutated in
    /// place via `record_usage` / `evaluate_thresholds`.
    /// `publish_from` no longer takes 4 sibling arguments for these.
    /// Phase 5b (2026-07-04): the `forced_runtime_route` field moved
    /// out of `CostGuard` (it's a routing decision, not spend
    /// tracking) into the sibling [`RouteState`] below.
    pub(crate) cost_guard: CostGuard,
    /// Per-run route decision state. Currently only holds the
    /// `forced_runtime_route` the cost guard installs when it
    /// downgrades to a cheaper model. Phase 5b (2026-07-04): extracted
    /// from `CostGuard::forced_runtime_route` — `CostGuard` is now
    /// strictly spend accounting.
    pub(crate) route_state: RouteState,
    /// Per-run message caches (last checkpoint snapshot + last
    /// assistant content with tool calls). Phase 5 (2026-07-04): the 2
    /// sibling `Option` fields that used to live here moved into
    /// [`MessageCache`] which is mutated in place via `store_*` /
    /// `take_*` / `*()` accessors. `publish_from` no longer takes 2
    /// sibling arguments for these.
    pub(crate) message_cache: MessageCache,
    /// Retries against invalid tool names / invalid tool JSON / truncated
    /// tool calls. Phase 5 (2026-07-03): the 3 sibling `u32` fields that
    /// used to live here moved into [`RetryCounters`] and are mutated
    /// in place via `bump_*` / `reset_*` methods. `publish_from` no
    /// longer takes 3 sibling arguments for these.
    pub(crate) retry_counters: RetryCounters,
    /// Context-pressure warning state (tier + cooldown timestamp + last %).
    /// Phase 5 (2026-07-03): extracted from 3 inline fields to consolidate
    /// the throttle/cooldown rules in [`ContextPressureMonitor::should_emit_warning`].
    pub(crate) context_pressure: ContextPressureMonitor,
}

/// Phase 1.3a (2026-07-03): outcome of [`AgentLoop::process_post_response`].
/// The shared post-LLM helper decides between two paths:
/// - `Finalize`: no tool calls → no work left, return the AgentResult.
/// - `Continue`: tool calls present → fall through to tool dispatch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PostResponseAction {
    /// No tool calls: stop the loop. The caller should `return`
    /// `Ok(self.finalize_agent_result(...))`.
    Finalize,
    /// Cost guard tripped (session spend exceeded `max_cost_usd`).
    /// Caller should `return` with `finalize_agent_result(.., false, false, None)`.
    CostGuardTripped,
    /// Tool calls present: caller falls through to `execute_tool_calls`.
    Continue,
}

// ---------------------------------------------------------------------------
// Backwards-compat re-exports (P2.2.4, 2026-06-30)
// ---------------------------------------------------------------------------

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermix_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        crate::session_lifecycle::hydrate_stored_system_prompt_from_hermes_home(config, hermix_home)
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    pub(crate) fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
    ) -> Vec<Message> {
        crate::session_lifecycle::messages_for_persisted_result(
            &self.config,
            ctx,
            persist_user_idx,
        )
    }

    pub(crate) fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
    ) -> AgentResult {
        self.finalize_agent_result(
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            false,
            true,
            None,
        )
    }

    pub(crate) fn should_emit_session_reset_hook(messages: &[Message]) -> bool {
        crate::session_lifecycle::should_emit_session_reset_hook(messages)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_agent_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finished_naturally: bool,
        interrupted: bool,
        finish_reason: Option<String>,
    ) -> AgentResult {
        crate::session_lifecycle::finalize_agent_result(
            &self.config,
            &self.focus_stack,
            self.memory_router.as_ref(),
            self.plugin_manager.as_ref(),
            &self.latest_compressed_summary,
            &self.latest_compression_archived_turns,
            &self.latest_compression_metrics,
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            finished_naturally,
            interrupted,
            finish_reason,
        )
    }

    /// Phase 1.3a (2026-07-03): the post-LLM logic that was duplicated
    /// ~200 lines in `run()` and `run_stream()`. Fires the PostLlmCall
    /// hook, accumulates usage, runs the cost guard (with auto-degrade
    /// to the configured cheap model), and decides between
    /// `Finalize` (no tool calls → caller returns) and `Continue`
    /// (tool calls → caller falls through to `execute_tool_calls`).
    ///
    /// `stream_mode` is `true` for `run_stream()` — the helper uses
    /// this to emit a stream-specific `tracing` line and to forward
    /// the `finish_reason` through the finalize path.
    ///
    /// Phase 5 (2026-07-03): 7 mutable scalar args collapsed to
    /// `&mut CostGuard` + `&mut Option<String>` (4 args shrunk to 1
    /// struct). `record_usage` / `evaluate_thresholds` / `install_downgrade`
    /// on [`CostGuard`] absorb the 4-field atomic-mutation, so the
    /// helper signature dropped from 11 args to 7. The two remaining
    /// mutable scalars — `last_content_with_tools` — stay on the
    /// helper signature because they're stored on `LoopState` (used by
    /// the codex-ack continuation logic on the next turn) and not on
    /// `CostGuard`.
    ///
    /// Phase 5 (2026-07-04 follow-up): `&mut Option<String>` for
    /// `last_content_with_tools` collapsed to `&mut MessageCache`,
    /// dropping the helper to 6 mutable args (the `Option<String>` is
    /// now reached via `cache.store_content_with_tools(...)` inside
    /// the helper). The companion helper `prepare_assistant_turn`
    /// also takes `&mut MessageCache` for the same reason.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_post_response(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        turn_usage_acc: Option<&UsageStats>,
        total_turns: u32,
        api_elapsed: u64,
        cost_guard: &mut CostGuard,
        route_state: &mut RouteState,
        message_cache: &mut MessageCache,
        stream_mode: bool,
    ) -> Result<PostResponseAction, AgentError> {
        crate::session_lifecycle::process_post_response(
            &self.config,
            self.plugin_manager.as_ref(),
            ctx,
            response,
            turn_usage_acc,
            total_turns,
            api_elapsed,
            cost_guard,
            route_state,
            message_cache,
            stream_mode,
            || self.resolve_cost_degrade_model(),
            |model| self.turn_route_cost_guard(model),
        )
    }

    /// Phase 2.2 (2026-07-03): shared per-turn post-LLM setup that
    /// `run()` and `run_stream()` previously open-coded after the
    /// `process_post_response` call. Builds the local `assistant_msg`,
    /// appends it to the context, updates `last_content_with_tools` if
    /// the assistant content came bundled with tool calls (used by the
    /// codex-ack continuation logic on the next turn), and reports
    /// whether any prior tool result exists in the history.
    ///
    /// Returns the three locals the caller needs:
    /// - `history_includes_tool`: did any earlier turn leave a `Tool`
    ///   message in the context? Used by
    ///   `decide_empty_response` to know whether tool
    ///   execution is part of the protocol context.
    /// - `assistant_msg`: the LLM's response, freshly appended to
    ///   the context.
    /// - `tool_calls`: the LLM's claimed tool calls, cloned for the
    ///   caller to dispatch / dedup.
    #[allow(clippy::type_complexity)]
    pub(crate) fn prepare_assistant_turn(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        message_cache: &mut MessageCache,
    ) -> (bool, Message, Option<Vec<ToolCall>>) {
        crate::session_lifecycle::prepare_assistant_turn(ctx, response, message_cache)
    }

    /// Phase 2.3 (2026-07-03): centralised `mute_post_response`
    /// control chunk emit. The same AtomicBool-swap-then-conditional-
    /// emit pattern was open-coded in two places inside `run_stream()`:
    /// the housekeeping block after tool dispatch (line ~5980) and the
    /// terminal finalize path (line ~5936). This helper takes the
    /// stream callback, the mute flag, and the desired next state, and
    /// only fires the control chunk when the state actually changes
    /// (the frontend only needs to know about transitions, not every
    /// dispatch). Both call sites become a single 1-line call.
    pub(crate) fn emit_mute_transition(
        on_chunk: &Box<dyn Fn(StreamChunk) + Send + Sync>,
        stream_mute: &std::sync::atomic::AtomicBool,
        next_muted: bool,
    ) {
        crate::session_lifecycle::emit_mute_transition(on_chunk, stream_mute, next_muted)
    }

    /// Phase 2.1 (2026-07-03): apply the four-variant
    /// `EmptyResponseAction` outcome produced by
    /// `crate::empty_response_handler::decide_empty_response`.
    /// Both `run()` and `run_stream()` previously open-coded this
    /// match arm — the only difference between the two call sites
    /// was the tracing message text. This helper centralises the
    /// match and parameterises the stream-mode log suffix.
    ///
    /// Returns `true` when a `ContinueWithUserMsg` or
    /// `ContinueWithSystemNudge` variant injected a message into the
    /// context — the caller should `continue;` to restart the loop
    /// with the injected message. Returns `false` when the variant
    /// was `PatchLastAssistant` (which mutated the last assistant
    /// message in place) or `Finalize` (a no-op) — the caller should
    /// fall through to its terminal finalize path.
    pub(crate) fn apply_fallback_outcome(
        &self,
        ctx: &mut ContextManager,
        action: crate::empty_response_handler::EmptyResponseAction,
        stream_mode: bool,
        total_turns: u32,
        autonomous_nudge_count: u32,
    ) -> bool {
        crate::session_lifecycle::apply_fallback_outcome(
            ctx,
            action,
            stream_mode,
            total_turns,
            autonomous_nudge_count,
        )
    }

    /// v2 (2026-06-21): most recent archive refs produced by
    /// `auto_compress_if_over_threshold`. Empty when the context has
    /// never crossed the 80% pressure threshold. Frontend uses this
    /// to render the "see archived context" affordance.
    pub fn last_archive_refs(&self) -> Vec<ArchiveRef> {
        self.last_archive_refs
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
            always_include: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "memory".to_string(),
            ],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            prompt_cache: std::sync::Mutex::new(crate::agent_helpers::PromptCache::default()),
            context_epoch: std::sync::atomic::AtomicU64::new(0),
            embedding_provider: None,
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
            always_include: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "memory".to_string(),
            ],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            prompt_cache: std::sync::Mutex::new(crate::agent_helpers::PromptCache::default()),
            context_epoch: std::sync::atomic::AtomicU64::new(0),
            embedding_provider: None,
        }
    }

    /// Attach a memory router for Phase 15 time-aware recall.
    pub fn with_memory_router(mut self, router: Arc<hermix_focus::MemoryRouter>) -> Self {
        self.memory_router = Some(router);
        self
    }

    /// Attach an embedding provider for vector recall in memory prefetch.
    pub fn with_embedding_provider(
        mut self,
        provider: Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>,
    ) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// M1: replace the per-loop `FocusStack<SessionFrame>`. Default
    /// is an empty stack (constructed in `new` / `with_interrupt`).
    /// Tests use this to inject a pre-populated stack; production
    /// callers should leave the default.
    pub fn with_focus_stack(mut self, stack: Arc<Mutex<FocusStack<SessionFrame>>>) -> Self {
        self.focus_stack = stack;
        self
    }

    /// M1.6.3: attach the consciousness tracker so the focus stack
    /// depth is reflected in the "ECG" stream the desktop renders.
    /// Called from `LocalAgentService::spawn_agent_loop`; tests
    /// typically leave the default (`None`).
    pub fn with_consciousness_tracker(
        mut self,
        tracker: Arc<crate::consciousness::ConsciousnessTracker>,
    ) -> Self {
        self.consciousness_tracker = Some(tracker);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    /// Attach a tool schema router. When set, the LLM only sees the
    /// router's output instead of the full tool registry (Phase 7 H1).
    /// Wrap your router with [`AlwaysInclude`] if you want a fixed
    /// must-have set to bypass the router.
    pub fn with_tool_router(mut self, router: Arc<dyn ToolSchemaRouter>) -> Self {
        self.tool_router = Some(router);
        self
    }

    /// Override the always-include tool whitelist.
    pub fn with_always_include(mut self, names: Vec<String>) -> Self {
        self.always_include = names;
        self
    }

    /// Attach an intent-aware prefetcher (Phase 15 H2).
    /// When set, the agent loop runs the prefetcher before each LLM call,
    /// dispatches Safe tools in parallel, and injects results as a
    /// system-prompt block.
    pub fn with_prefetcher(
        mut self,
        prefetcher: Prefetcher,
        learned_stats: hermix_intelligence::prefetch::IntentStats,
    ) -> Self {
        self.prefetcher = Some(Arc::new(prefetcher));
        self.learned_prefetch_stats = Arc::new(Mutex::new(learned_stats));
        self
    }

    /// Attach an advanced LLM-powered context compressor. When set,
    /// `auto_compress_if_over_threshold` uses this instead of the
    /// simple character-based compressor, producing structured
    /// summaries that preserve goals, progress, decisions, and files.
    pub fn with_compressor(
        mut self,
        compressor: crate::compression::ContextCompressor,
    ) -> Self {
        self.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));
        self
    }

    // -- M5 ACUI emit ------------------------------------------------------

    /// Emit an ACUI directive to upstream listeners (if any).
    ///
    /// No-op if no `on_acui_directive` callback was wired (e.g. unit
    /// tests, batched runs). The directive is JSON-serialized and passed
    /// to the callback as a string; the upstream layer is responsible
    /// for deserializing and forwarding it as a `StreamEvent::Acui`.
    ///
    /// This method never blocks and never returns an error — the ACUI
    /// pipeline is best-effort. A failure in the callback is logged
    /// and swallowed (the agent keeps running).
    pub fn emit_acui(&self, directive: AcuiDirective) {
        crate::acui_dispatch::emit_acui(&self.callbacks, directive)
    }

    /// Convenience: emit a `Show` directive.
    pub fn emit_acui_show(
        &self,
        id: impl Into<String>,
        kind: impl Into<String>,
        props: serde_json::Value,
    ) {
        crate::acui_dispatch::emit_acui_show(&self.callbacks, id, kind, props)
    }

    /// Convenience: emit an `Update` directive.
    pub fn emit_acui_update(&self, id: impl Into<String>, patch: serde_json::Value) {
        crate::acui_dispatch::emit_acui_update(&self.callbacks, id, patch)
    }

    /// Convenience: emit a `Hide` directive.
    pub fn emit_acui_hide(&self, id: impl Into<String>) {
        crate::acui_dispatch::emit_acui_hide(&self.callbacks, id)
    }

    /// Build a default router (KeywordRouter + AlwaysInclude).
    /// Threads `self.always_include` through so user-configured
    /// always-include names actually reach the `AlwaysInclude` wrapper.
    /// Pre-2026-07-04: this method took no `self` and ignored the
    /// field — `always_include` was a write-only field.
    pub fn default_tool_router(&self) -> Arc<dyn ToolSchemaRouter> {
        crate::acui_dispatch::default_tool_router(&self.always_include)
    }

    /// Resolve the tool schemas to send to the LLM this turn.
    ///
    /// Behavior:
    /// - If `tool_router` is `None`: return `candidates` unchanged.
    /// - If `tool_router` is `Some`: build a [`TurnContext`] from the
    ///   last user message + recent history, call `route`, and return
    ///   the routed schemas. The `always_include` whitelist is enforced
    ///   by the router itself if the caller wrapped it with
    ///   [`AlwaysInclude`]; this method does not re-wrap.
    pub fn route_tools(
        &self,
        candidates: Vec<ToolSchema>,
        messages: &[Message],
    ) -> Vec<ToolSchema> {
        let Some(router) = self.tool_router.as_ref() else {
            return candidates;
        };
        let user_message = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let recent: Vec<Message> = messages
            .iter()
            .rev()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let ctx = TurnContext {
            user_message,
            recent_messages: recent,
            active_toolsets: self.config.active_toolsets.clone(),
            max_tools: candidates.len().min(self.config.max_routed_tools),
        };
        let routed: Vec<ToolSchema> = router
            .route(&ctx, candidates)
            .into_iter()
            .map(|r| r.schema)
            .collect();
        // Progressive disclosure: if deferred tools exceed threshold,
        // replace them with tool_search/tool_describe/tool_call bridges.
        let assembly = hermix_tools::assemble_tool_schemas(routed);
        if assembly.activated {
            tracing::info!(
                deferred = assembly.deferred_count,
                "tool_search: {} deferred tools behind bridge",
                assembly.deferred_count,
            );
        }
        assembly.tool_schemas
    }

    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    ///
    /// Returns `false` when the text is empty/whitespace-only.
    pub fn steer(&self, text: &str) -> bool {
        crate::steer::steer(&self.pending_steer, text)
    }

    /// Clear any queued `/steer` guidance without applying it.
    pub fn clear_pending_steer(&self) {
        crate::steer::clear_pending_steer(&self.pending_steer)
    }

    // ── M1: FocusStack plumbing ────────────────────────────────────
    //
    // The agent loop owns a per-session `FocusStack<SessionFrame>`.
    // Tool-dispatch paths can call `note_delegation_start` to push a
    // frame for each `delegate_task` invocation and
    // `note_delegation_end` to pop it on completion. Popped
    // `Cleared` frames generate a `FocusConclusion` that is
    // buffered in `pending_conclusions` until
    // `drain_focus_conclusions_to_memory_router` ships them to the
    // long-term memory backend.

    /// Push a `SessionRoot` frame for the current session. Idempotent
    /// — a second call is a no-op if a root frame is already on the
    /// stack. Callers should invoke this once at `run()` start.
    pub fn note_session_started(&self, task: impl Into<String>) {
        crate::focus_stack_ops::note_session_started(&self.focus_stack, task);
    }

    /// Push a `Delegation` frame for a `delegate_task` invocation.
    /// `sub_agent_id` is the UUID the orchestrator / sub-agent
    /// returns; pass an empty string if not yet known (it will be
    /// matched by depth in `note_delegation_end`).
    ///
    /// Returns the frame id, or `None` if the lock is poisoned.
    pub fn note_delegation_start(
        &self,
        task: impl Into<String>,
        sub_agent_id: impl Into<String>,
    ) -> Option<hermix_focus::FocusFrameId> {
        crate::focus_stack_ops::note_delegation_start(
            &self.focus_stack,
            self.delegate_depth,
            task,
            sub_agent_id,
        )
    }

    /// Pop the topmost `Delegation` frame whose payload matches
    /// `sub_agent_id` (or the topmost delegation frame if
    /// `sub_agent_id` is empty). The popped frame is `mark_clear`-ed
    /// first so the next `pop` generates a `FocusConclusion`.
    ///
    /// Returns the depth the popped frame was at, or `None` if no
    /// matching frame was on the stack.
    pub fn note_delegation_end(
        &self,
        sub_agent_id: &str,
        status: SessionFrameStatus,
    ) -> Option<u32> {
        crate::focus_stack_ops::note_delegation_end(&self.focus_stack, sub_agent_id, status)
    }

    /// Current depth of the focus stack (number of frames on it).
    /// Returned as `u8` because the consciousness tracker's
    /// `set_focus_depth` takes `u8` and the stack never grows past
    /// `MAX_DELEGATE_DEPTH` (typically 5) in practice.
    pub fn current_focus_depth(&self) -> u8 {
        crate::focus_stack_ops::current_focus_depth(&self.focus_stack)
    }

    /// Drain pending `FocusConclusion`s from the focus stack and
    /// forward them to the long-term memory backend. Returns the
    /// number of conclusions processed.
    ///
    /// No-op if `memory_router` is `None` (conclusions are still
    /// drained from the focus stack — they don't pile up).
    ///
    /// M1.6.4: this now actually persists via the router's
    /// `upsert_conclusion` fan-out. Backends that don't implement
    /// the write surface (8 HTTPS plugins) return
    /// `MemoryError::NotImplemented` and the router skips them
    /// silently. Only the embedded SQLite backend (and any future
    /// write-capable backend) actually writes to long-term store.
    pub fn drain_focus_conclusions_to_memory_router(&self) -> usize {
        let session_id = self.config.session_id.clone().unwrap_or_default();
        crate::memory_prefetch::drain_focus_conclusions(
            &self.focus_stack,
            self.memory_router.as_ref(),
            &session_id,
        )
    }

    pub(crate) fn drain_pending_steer(&self) -> Option<String> {
        crate::steer::drain_pending_steer(&self.pending_steer)
    }

    pub(crate) fn restash_pending_steer(&self, text: String) {
        crate::steer::restash_pending_steer(&self.pending_steer, text)
    }

    /// Pre-LLM-call drain: inject steer into the last tool result if possible; otherwise restash.
    pub(crate) fn pre_api_apply_pending_steer(&self, ctx: &mut ContextManager) {
        crate::steer::pre_api_apply_pending_steer(&self.pending_steer, ctx)
    }

    /// Post-tool-call apply: inject steer into the last tool result if pending.
    pub(crate) fn post_tool_apply_pending_steer(&self, ctx: &mut ContextManager) {
        crate::steer::post_tool_apply_pending_steer(&self.pending_steer, ctx)
    }

    // -- Plugin hook helpers ------------------------------------------------

    pub(crate) fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        crate::hook_dispatch::invoke_hook(self.plugin_manager.as_ref(), hook, ctx_val)
    }

    pub(crate) fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        crate::hook_dispatch::inject_hook_context(results, ctx)
    }

    // -- Memory helpers ----------------------------------------------------

    pub(crate) async fn memory_prefetch(&self, query: &str, session_id: &str, messages: Option<&[Message]>) -> String {
        crate::memory_prefetch::memory_prefetch(
            &self.config,
            self.memory_router.as_deref(),
            self.embedding_provider.as_deref(),
            &self.memory_recall_seeded,
            self.config.hermix_home.as_deref().map(std::path::Path::new),
            query,
            session_id,
            messages,
        )
        .await
    }

    pub(crate) fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        crate::memory_prefetch::memory_sync(
            &self.config,
            self.memory_router.as_deref(),
            user,
            assistant,
            session_id,
        )
    }

    /// Phase 18 (架构-1): freeze the current session state into a
    /// `session_snapshots` row. Called every N turns (default 50)
    /// and on `on_session_end`. The `frame_ids` and `summary` are
    /// extracted from the router's embedded backend so this method
    /// is a no-op when the embedded backend is absent.
    ///
    /// Designed to be cheap: the embedded SQLite path is O(visible
    /// frames) on `total_turns.is_multiple_of(N)` boundary calls,
    /// and O(1) on all other turns.
    pub(crate) fn maybe_freeze_snapshot(&self, session_id: &str, turn_number: u32) {
        crate::memory_prefetch::maybe_freeze_snapshot(
            &self.config,
            self.memory_router.as_deref(),
            session_id,
            turn_number,
        )
    }

    /// Phase 18 (架构-2): enforce a hard cap on visible frames so
    /// memory.db doesn't grow unbounded. Called every 100 turns as a
    /// background cleanup. Default cap is 10000 — enough for ~3
    /// months of heavy chat, soft-hidden rows stay on disk for the
    /// audit trail and the 30-day janitor.
    pub(crate) fn maybe_enforce_frame_cap(&self, turn_number: u32) {
        crate::memory_prefetch::maybe_enforce_frame_cap(
            &self.config,
            self.memory_router.as_deref(),
            turn_number,
        )
    }

    pub(crate) fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        crate::memory_prefetch::notify_memory_writes(
            &self.config,
            self.memory_router.as_deref(),
            tool_calls,
            results,
        )
    }

    pub(crate) fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        crate::memory_prefetch::notify_delegations(
            &self.config,
            self.memory_router.as_ref(),
            &self.focus_stack,
            tool_calls,
            results,
            self.consciousness_tracker.as_ref(),
        );
    }

    pub(crate) fn memory_on_turn_start(&self, turn: u32, message: &str) {
        crate::memory_prefetch::memory_on_turn_start(
            &self.config,
            self.memory_router.as_deref(),
            turn,
            message,
        );
    }

    pub(crate) fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        crate::memory_prefetch::memory_pre_compress_note(
            &self.config,
            self.memory_router.as_deref(),
            messages,
        )
    }

    pub(crate) fn memory_on_session_end(&self, messages: &[Message]) {
        crate::memory_prefetch::memory_on_session_end(
            &self.config,
            self.memory_router.as_ref(),
            &self.focus_stack,
            messages,
        );
    }

    /// M1: collect the typed, route-gated directions that will be
    /// evaluated against the active route context and rendered into the
    /// system prompt. Each direction is one inline `if/else` branch
    /// extracted from the previous monolith of `build_system_prompt` —
    /// see `context::DirRoute` for the gate logic.
    pub(crate) fn extract_provider_and_model<'a>(&self, model: &'a str) -> (String, &'a str) {
        crate::prompt_assembly::extract_provider_and_model(&self.config, model)
    }

    pub(crate) fn auth_tokens_path(&self) -> PathBuf {
        crate::runtime_provider::auth_tokens_path(&self.config)
    }

    pub(crate) fn build_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
        api_mode: Option<&ApiMode>,
        credential_pool: Option<&Arc<CredentialPool>>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        crate::runtime_provider::build_runtime_provider(
            &self.config,
            &self.oauth_refresh,
            provider,
            model_name,
            route_base_url,
            api_key_env_override,
            explicit_api_key,
            api_mode,
            credential_pool,
        )
    }

    pub(crate) fn credential_pool_for_route<'a>(
        &'a self,
        rt: &'a TurnRuntimeRoute,
    ) -> Option<&'a Arc<CredentialPool>> {
        crate::runtime_provider::credential_pool_for_route(self.primary_credential_pool.as_ref(), rt)
    }

    pub(crate) fn messages_for_api_call(&self, ctx: &ContextManager) -> Vec<Message> {
        crate::prompt_assembly::messages_for_api_call(&self.config, ctx)
    }

/// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    pub(crate) fn resolve_initial_system_prompt(
        &self,
        user_message: &str,
        tool_schemas: &[ToolSchema],
        tool_call_count: usize,
    ) -> (String, bool) {
        crate::prompt_assembly::resolve_initial_system_prompt(
            &self.config,
            &self.prompt_cache,
            self.memory_router.as_deref(),
            &self.callbacks,
            user_message,
            tool_schemas,
            tool_call_count,
        )
    }

    /// Compress when total chars exceed 80% of the context budget (Python auto-compaction).
    ///
    /// Uses the advanced LLM-powered compressor when available (set via
    /// `with_compressor`), falling back to the simple character-based
    /// compressor otherwise.
    ///
    /// v2 (2026-06-21): before the LLM compressor replaces the middle
    /// of the conversation with a summary, we **archive** the same
    /// slice to disk via [`archive_context_section`]. The summary
    /// becomes lossy-by-design; the archive file is the lossless
    /// evidence the user can re-open from the chat UI. The archive
    /// refs are stashed in `last_archive_refs` for the frontend.
    ///
    /// v3 (2026-06-29): also emit `compaction_status` snapshots at
    /// the pressure-detected and post-attempt boundaries so the
    /// `CompactionStatusBanner` can render in real time. We clone
    /// the snapshot before dropping the compressor lock so the
    /// callback can fire on the upstream IO task without holding
    /// the compressor's `MutexGuard`.
    pub(crate) async fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        crate::compression_ops::auto_compress_if_over_threshold(
            &self.config,
            &self.callbacks,
            self.compression.as_ref(),
            &self.last_archive_refs,
            &self.latest_compressed_summary,
            &self.latest_compression_archived_turns,
            &self.latest_compression_metrics,
            ctx,
            |msgs| self.memory_pre_compress_note(msgs),
        )
        .await
    }

    pub(crate) fn emit_status(&self, event_type: &str, message: &str) {
        crate::message_helpers::emit_status(&self.config, &self.callbacks, event_type, message)
    }

    /// 2026-06-29: forward a `BreakerSnapshot` to whoever wired
    /// `AgentCallbacks::on_compaction_status`. The snapshot already
    /// carries every field the front-end `CompactionStatusBanner`
    /// needs (state, pressure, failure reason, change timestamp)
    /// so the upstream layer just JSON-serialises it and emits the
    /// `compaction_status` WS event.
    ///
    /// `quiet_mode` does NOT short-circuit this path: the banner is
    /// a UI affordance, and suppressing it would hide legitimate
    /// "context is overflowing" / "compression has been disabled"
    /// signals from the user.
    pub(crate) fn emit_compaction_status(&self, snapshot: &hermix_core::CompressionEngineSnapshot) {
        crate::message_helpers::emit_compaction_status(&self.callbacks, snapshot)
    }

    pub(crate) fn assistant_visible_text(m: &Message) -> bool {
        crate::message_helpers::assistant_visible_text(m)
    }

    pub(crate) fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        crate::message_helpers::assistant_visible_text_after_think_blocks(m)
    }

    pub(crate) fn assistant_has_reasoning(m: &Message) -> bool {
        crate::message_helpers::assistant_has_reasoning(m)
    }

    pub(crate) fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
        crate::message_helpers::normalize_tool_call_arguments(tc)
    }

    pub(crate) fn extra_body_for_api_mode(&self, api_mode: &ApiMode) -> Option<Value> {
        crate::message_helpers::extra_body_for_api_mode(&self.config, api_mode)
    }

    // -- Retry-aware LLM call ---------------------------------------------

    pub(crate) fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermix_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, route))
    }

    pub(crate) async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
    ) -> Result<hermix_core::LlmResponse, AgentError> {
        crate::llm_retry::call_llm_with_retry(
            &self.config,
            &self.oauth_refresh,
            &self.callbacks,
            &self.llm_provider,
            self.plugin_manager.as_ref(),
            self.primary_credential_pool.as_ref(),
            &self.interrupt,
            ctx,
            tool_schemas,
            route,
        )
        .await
    }

    pub(crate) fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        crate::stream_collect::assemble_stream_assistant_message(content, reasoning_content, tool_calls)
    }

    /// Apply one streaming [`StreamChunk`] during [`Self::collect_stream_llm_response`].
    ///
    /// `think_parser` is a stateful splitter that pulls `<think>...</think>` text
    /// out of the visible content stream in real time and routes it to
    /// `reasoning_content` instead. Without this, providers that embed their
    /// reasoning in `delta.content` (MiniMax M2.7, Qwen, DeepSeek-R1) would
    /// leak the whole thinking monologue into the user-visible reply.
    ///
    /// `tool_call_sanitizer` is a stateful redactor that pulls raw
    /// `<minimax:tool_call>…</minimax:tool_call>` /
    /// `<invoke name="…">…</invoke>` blocks out of the *clean* (post-think)
    /// text. Without it, providers that emit tool calls as raw XML inside
    /// `delta.content` (MiniMax, older Hermes fine-tunes) would leak the
    /// whole XML envelope to the user. See
    /// `hermix_core::tool_call_sanitizer` for the full state machine.
    pub(crate) fn apply_stream_collect_chunk(
        &self,
        chunk: StreamChunk,
        content: &mut String,
        raw_content: &mut String,
        reasoning_content: &mut String,
        think_parser: &mut ThinkBlockParser,
        tool_call_sanitizer: &mut ToolCallSanitizer,
        tool_calls: &mut Vec<ToolCall>,
        last_usage: &mut Option<UsageStats>,
        finish_reason: &mut Option<String>,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) {
        crate::stream_collect::apply_stream_collect_chunk(
            &self.callbacks,
            chunk,
            content,
            raw_content,
            reasoning_content,
            think_parser,
            tool_call_sanitizer,
            tool_calls,
            last_usage,
            finish_reason,
            on_chunk,
        );
    }

    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
    pub(crate) async fn collect_stream_llm_response(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        active_model: &str,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<StreamCollectOutcome, AgentError> {
        crate::stream_collect::collect_stream_llm_response(
            &self.config,
            &self.oauth_refresh,
            &self.callbacks,
            &self.llm_provider,
            self.primary_credential_pool.as_ref(),
            &self.interrupt,
            ctx,
            tool_schemas,
            route,
            active_model,
            on_chunk,
        )
        .await
    }
    pub(crate) async fn prepare_run_context(
        &self,
        mut messages: Vec<Message>,
        tools: Option<&Vec<ToolSchema>>,
    ) -> PreparedRun {
        let mut ctx = ContextManager::for_model_with_providers(
            &self.config.model,
            Some(&self.config.llm_providers),
        );

        // Sanitize surrogates and strip prior budget warnings from history
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Determine which tools to expose (Phase 7 H1: route through tool_router
        // when configured; otherwise send the full registry as before).
        let tool_schemas: Vec<ToolSchema> = match tools {
            Some(t) => self.route_tools(t.clone(), &messages),
            None => {
                let all = self.tool_registry.schemas();
                self.route_tools(all, &messages)
            }
        };
        // Log the *resolved* tool count (after routing), not the raw
        // parameter. The `tool_count` on the span is the caller's
        // argument and is zero when the caller passes `None`. Was
        // previously open-coded in `run_stream` only; centralizing
        // here so both `run` and `run_stream` emit the same diagnostic.
        if tool_schemas.is_empty() {
            tracing::warn!(
                registry_size = self.tool_registry.schemas().len(),
                "No tool schemas after routing — LLM will not see any tools"
            );
        } else {
            tracing::info!(
                resolved = tool_schemas.len(),
                "Tool schemas resolved for this turn"
            );
        }

        // Build and inject system prompt (or reuse SQLite-cached prompt for session continuity)
        let tool_call_count = ctx.get_messages()
            .iter()
            .filter(|m| m.tool_calls.is_some())
            .count();
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas, tool_call_count);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .rfind(|(_, m)| m.role == MessageRole::User)
                .map(|(i, _)| i)
        } else {
            None
        };
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut codex_ack_continuations: u32 = 0;
        // Counts how many times the autonomous loop has re-driven the LLM after
        // it produced plain text without tool calls. Capped at
        // `default_max_autonomous_nudges()` to prevent infinite loops when the
        // model genuinely cannot make further progress.
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut autonomous_nudge_count: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch for first user message
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let first_user = ctx
            .get_messages()
            .iter()
            .rfind(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id, Some(ctx.get_messages())).await;
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        // Phase 18 (BaiLongma gatherer): pre-fetch file context referenced
        // in the user message so the LLM doesn't need to call read_file
        // on the first turn. Best-effort; failures are silently ignored.
        if !first_user.is_empty() {
            let gathered = crate::gatherer::gather_context(
                &first_user,
                self.memory_router.as_ref(),
                None,
            )
            .await;
            if let Some(gather_msg) = crate::gatherer::format_gathered_context(&gathered) {
                ctx.add_message(gather_msg);
            }
        }

        if self.config.preflight_context_compress {
            self.auto_compress_if_over_threshold(&mut ctx).await;
        }

        PreparedRun {
            ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            codex_ack_continuations,
            autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        }
    }


    /// Run the agent loop with streaming.
    ///
    /// Uses the LLM provider's streaming API and invokes `on_chunk` for each
    /// incremental delta. The stream is collected into a complete response
    /// before tool execution proceeds.
    /// Remove duplicate tool calls that share the same function name and arguments.
    pub(crate) fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        crate::tool_dispatch::deduplicate_tool_calls(calls)
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    pub(crate) fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        crate::tool_dispatch::repair_tool_call(&self.tool_registry, tc)
    }

    /// Inject current session id into `session_search` calls when absent.
    pub(crate) fn hydrate_session_search_args(&self, tc: &mut ToolCall) {
        crate::tool_dispatch::hydrate_session_search_args(self.config.session_id.as_deref(), tc)
    }

    pub(crate) fn turn_route_cost_guard(&self, model: String) -> TurnRuntimeRoute {
        crate::cost_routing::turn_route_cost_guard(
            &self.config,
            self.primary_credential_pool.as_ref(),
            model,
        )
    }

    pub(crate) fn resolve_smart_runtime_route(&self, messages: &[Message]) -> Option<TurnRuntimeRoute> {
        crate::cost_routing::resolve_smart_runtime_route(
            &self.config,
            &self.oauth_refresh,
            self.primary_credential_pool.as_ref(),
            messages,
        )
    }

    pub(crate) fn resolve_cost_degrade_model(&self) -> Option<String> {
        crate::cost_routing::resolve_cost_degrade_model(&self.config)
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    pub(crate) async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        crate::session_lifecycle::handle_max_iterations(
            &self.config,
            &self.llm_provider,
            ctx,
        )
        .await
    }

    /// Build a per-tool hint that we append to a user-denial reason so the
    /// LLM, on its next turn, has a concrete suggestion for *which* tool
    /// would have been more appropriate.
    ///
    /// ## Why this exists
    ///
    /// Regression (2026-06-14): user said "看下 D:\data 下有啥". The model
    /// chose `computer_use` with `action=screenshot`. The user denied.
    /// The model then claimed "I have no terminal / list_files / read_file
    /// tool" — even though all three are registered and pinned to the top
    /// of the `<tools>` block.
    ///
    /// Two layers of defense had been added (descriptions + system-prompt
    /// reminder) but neither was anchored to the deny path. By the time
    /// the user hits Deny, the model has already committed to its choice;
    /// a plain "User denied the tool call." gives it no signal to re-pick.
    /// The hint below is the *third* defense: when the user rejects,
    /// surface the right next move in the very tool-result message the
    /// model will read.
    ///
    /// ## Design constraints
    ///
    /// - **Short** — the tool-result string is fed back to the LLM, and
    ///   every extra token here competes with the user's original prompt
    ///   in the next turn. We aim for one sentence, not a paragraph.
    /// - **Tool-specific** — generic "consider another tool" hints are
    ///   too vague. We map the most-mis-picked tools to the right
    ///   replacement so the LLM has a concrete noun to call.
    /// Resolve the pre-tool-dispatch hook for every tool call up front.
    ///
    /// When `AgentCallbacks::on_pre_tool_dispatch` is set, this invokes the
    /// hook once per tool call. `Allow` and `Deny` decisions resolve
    /// synchronously; `AwaitApproval` blocks on a `oneshot::Receiver` until
    /// the upstream layer (e.g. `LocalAgentService`) sends a verdict. The
    /// returned map is keyed by `ToolCall::id` and consumed by both the
    /// orchestrated `delegate_task` path and the `JoinSet` path.
    pub(crate) async fn resolve_pre_dispatch(
        &self,
        tool_calls: &[ToolCall],
    ) -> HashMap<String, Result<(), String>> {
        crate::tool_dispatch::resolve_pre_dispatch(
            self.callbacks.on_pre_tool_dispatch.as_ref().map(|b| b.as_ref()),
            tool_calls,
        )
        .await
    }

    pub(crate) fn resolve_max_delegate_depth(&self) -> u32 {
        crate::tool_dispatch::resolve_max_delegate_depth()
    }

    /// Cap concurrent delegate_task calls based on config.
    pub(crate) fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
        crate::tool_dispatch::cap_delegates(self.config.max_concurrent_delegates, tool_calls)
    }

    pub(crate) fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config.background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermix_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Phase 2.4: terminal-persistence shared by `run()` and `run_stream()`
    /// natural-finish paths. Extracted to keep the two call sites in
    /// lock-step (P3 added `maybe_freeze_snapshot` to run_stream and we
    /// don't want that fix to drift again).
    pub(crate) fn persist_session_terminal(
        &self,
        ctx: &ContextManager,
        session_id: &str,
        total_turns: u32,
        review_memory_at_end: bool,
    ) {
        let (u, a) = extract_last_user_assistant(ctx.get_messages());
        self.memory_sync(&u, &a, session_id);
        // Phase 18: freeze a final snapshot at session end so the next
        // session can pick up without replaying every frame.
        self.maybe_freeze_snapshot(session_id, total_turns);
        self.spawn_background_review(total_turns, ctx, review_memory_at_end);
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    pub(crate) fn spawn_background_review(&self, turn: u32, ctx: &ContextManager, review_memory_at_end: bool) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config.background_review_enabled {
            return;
        }
        let mut review_skills = false;
        // P0-1: only fire the skill review when the user hasn't
        // opted out via `auto_skill_extraction: false` in
        // ~/.hermix/config.yaml. Without this guard, the master
        // off-switch would be ignored by the background loop.
        if self.config.auto_skill_extraction
            && self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                if c.iters_since_skill >= self.config.skill_creation_nudge_interval {
                    review_skills = true;
                    c.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = self.config.clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        // P0-1: child session must NOT write skills back to the
        // user's primary skill library — the background-review
        // task is itself a child of the parent's agent loop, and
        // a misbehaving reflection would pollute
        // `~/.hermix/skills/`. Skill creation only happens at the
        // top-level agent.
        cfg.auto_skill_extraction = false;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.max_turns = cfg.max_turns.min(8);
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let review_cb = self.callbacks.background_review_callback.clone();
        tokio::spawn(async move {
            let agent = AgentLoop::new(cfg, tools, provider);
            match agent.run(hist, None).await {
                Ok(result) => {
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summarize_background_review_result(&result.messages)
                        {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    pub(crate) fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }

    /// See [`PostLlmAction`]. Mutates `inner_thinking` / `inner_empty`
    /// when bumping the per-attempt retry budget and emits a `lifecycle`
    /// status line on each transition.
    pub(crate) fn classify_post_llm_response(
        &self,
        r: hermix_core::LlmResponse,
        inner_thinking: &mut u32,
        inner_empty: &mut u32,
        ctx: &mut ContextManager,
    ) -> PostLlmAction {
        crate::session_lifecycle::classify_post_llm_response(
            &self.config,
            &self.callbacks,
            r,
            inner_thinking,
            inner_empty,
            ctx,
        )
    }
}

/// File-change summary emitted after a successful mutating tool call.
/// Used by the chat UI to render a WorkBuddy-style "文件变更 (N个)" row
/// inline in the conversation flow.
#[derive(Debug, Clone)]
pub(crate) struct FileChangeInfo {
    pub path: String,
    pub change_type: &'static str, // "created" | "modified" | "deleted" | "renamed"
    pub summary: Option<String>,
}

/// Phase 4.1 (2026-07-03): the per-attempt classification that sits
/// inside the `let response = loop { ... }` retry block in both
/// `run()` and `run_stream()`. Identical ~40 lines in both call
/// sites — has_tools / visible_text / reasoning-retry / empty-retry /
/// exhausted. Extracted here so the two retry loops can keep their
/// own transport (sync vs stream-first) and call sites can `match`
/// on the resulting action.
///
/// The helper takes `r` by value because the caller is going to
/// hand ownership of the response to either a downstream consumer
/// (`Break(r)`) or the context (the reasoning-retry branch already
/// pushed `r.message` onto `ctx` before returning `RetryReasoning`,
/// so `r` itself is dropped on the caller side).
///
/// `pub(crate)` so the `mod tests` block below can `match` on the
/// variants; an impl-block-local enum would be private to the
/// `AgentLoop` impl and unreachable from tests.
#[derive(Debug)]
pub(crate) enum PostLlmAction {
    /// LLM produced tool calls or visible text — leave the retry
    /// loop with this response.
    Break(hermix_core::LlmResponse),
    /// Reasoning-only response; the message has been pushed onto
    /// `ctx` and the caller should re-enter the retry loop.
    RetryReasoning,
    /// Empty response (no reasoning, no content, no tools); the
    /// caller should re-enter the retry loop without touching
    /// `ctx`.
    RetryEmpty,
}

/// Look at a completed tool call and decide whether it produced a
/// filesystem change worth surfacing in the chat summary. We only
/// handle the three mutating tools (write_file / patch / search_files),
/// and we only emit on success.
pub(crate) fn extract_file_change(
    tool_name: &str,
    arguments: &str,
    output: &str,
) -> Option<FileChangeInfo> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let path = match tool_name {
        "write_file" | "patch" => args.get("path")?.as_str()?.to_string(),
        "search_files" => return None, // read-only
        _ => return None,
    };
    if path.is_empty() {
        return None;
    }
    // Heuristic: write_file to a new path = "created", overwrite = "modified".
    // patch is always "modified". We can't always tell from the tool
    // output alone, so we default to "modified" for write_file (the
    // common case in our agent flow) and the operator can refine later.
    let change_type = match tool_name {
        "write_file" => "modified",
        "patch" => "modified",
        _ => "modified",
    };
    // Pull a brief summary from the tool output's first non-empty line
    // (most write_file handlers return "<N bytes written>" or similar).
    let summary = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect::<String>());
    Some(FileChangeInfo {
        path,
        change_type,
        summary,
    })
}

/// Extract the last user and assistant content from a message slice for memory sync.
pub(crate) fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    let mut actions: Vec<String> = Vec::new();
    for msg in messages {
        if !matches!(msg.role, hermix_core::MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if !data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            continue;
        }
        let lower = message.to_ascii_lowercase();
        if lower.contains("created") || lower.contains("updated") {
            actions.push(message);
        } else if lower.contains("added") || (!target.is_empty() && lower.contains("add")) {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if message.contains("Entry added") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if lower.contains("removed") || lower.contains("replaced") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("💾 {}", deduped.join(" · ")))
}

// Physically moved to `cost_guard.rs` (2026-07-04) alongside the
use crate::cost_guard::merge_usage;

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::JsonSchema;
    use crate::cost_guard::{WarnEffect, estimate_usage_cost_usd};

    #[test]
    pub(crate) fn p1_loop_state_defaults_to_zero() {
        // Phase 1 (2026-07-03): `LoopState::default` must produce a
        // zero-initialized struct that is `#[derive(Default)]` so the
        // refactor of `run()` / `run_stream()` can keep using `..Default::default()`.
        //
        // Phase 5 (2026-07-04): `total_api_time_ms` / `total_tool_time_ms`
        // moved into `state.timings`; access path now `s.timings.*`.
        // `last_checkpoint_messages` / `last_content_with_tools` moved
        // into `state.message_cache`; access path now
        // `s.message_cache.checkpoint().is_none()` /
        // `s.message_cache.last_content_with_tools.is_none()`.
        let s = LoopState::default();
        assert_eq!(s.total_turns, 0);
        assert_eq!(s.timings.total_api_time_ms, 0);
        assert_eq!(s.timings.total_tool_time_ms, 0);
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        assert!(s.route_state.forced_runtime_route.is_none());
        assert!(s.message_cache.checkpoint().is_none());
        assert_eq!(s.retry_counters.invalid_tool, 0);
        assert_eq!(s.retry_counters.invalid_json, 0);
        assert_eq!(s.retry_counters.truncated_tool, 0);
        assert!(s.message_cache.last_content_with_tools.is_none());
        assert_eq!(s.context_pressure.warned_tier, 0.0);
        assert!(s.context_pressure.last_warn_at.is_none());
        assert_eq!(s.context_pressure.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn p1_loop_state_counters_increment_independently() {
        // Sanity: the per-turn counters can be updated without touching
        // unrelated state — required because `run()` and `run_stream()`
        // will both mutate these fields on every turn.
        //
        // Phase 5 (2026-07-04): timings now use `state.timings.record_*`
        // (saturating-add) instead of a top-level `+= 1`. We exercise
        // the new path here and assert it doesn't bleed into other
        // counters.
        let mut s = LoopState::default();
        s.total_turns += 1;
        s.retry_counters.bump_invalid_tool();
        s.retry_counters.bump_invalid_json();
        s.retry_counters.bump_invalid_json();
        s.cost_guard.session_cost_usd += 0.001;
        s.timings.record_api(42);
        s.timings.record_tool(99);
        assert_eq!(s.total_turns, 1);
        assert_eq!(s.retry_counters.invalid_tool, 1);
        assert_eq!(s.retry_counters.invalid_json, 2);
        assert!(s.cost_guard.session_cost_usd > 0.0);
        // Unrelated fields stay zero.
        assert_eq!(s.timings.total_api_time_ms, 42);
        assert_eq!(s.timings.total_tool_time_ms, 99);
        assert!(s.cost_guard.accumulated_usage.is_none());
    }

    #[test]
    pub(crate) fn p5_turn_timings_record_independently() {
        // Phase 5 (2026-07-04): `record_api` and `record_tool` must
        // accumulate independently (each on its own `u64`), saturating
        // on overflow (a long session shouldn't wrap around). Default
        // must be all zeros so `LoopState::default()` semantics stay
        // unchanged from the pre-extraction two-`u64`-field version.
        let mut t = TurnTimings::default();
        assert_eq!(t.total_api_time_ms, 0);
        assert_eq!(t.total_tool_time_ms, 0);

        t.record_api(100);
        t.record_api(50);
        t.record_tool(200);
        assert_eq!(t.total_api_time_ms, 150);
        assert_eq!(t.total_tool_time_ms, 200);

        // Saturating: u64::MAX + 1 stays at u64::MAX instead of wrapping
        // to 0 (which would silently under-report a long session).
        let mut t2 = TurnTimings::default();
        t2.record_api(u64::MAX);
        t2.record_api(1);
        assert_eq!(t2.total_api_time_ms, u64::MAX);
    }

    #[test]
    pub(crate) fn p5_message_cache_store_and_take_independently() {
        // Phase 5 (2026-07-04): `store_checkpoint` /
        // `store_content_with_tools` / `take_content_with_tools` /
        // `checkpoint` / `content_with_tools` must all behave
        // independently (the two caches are unrelated fields, not
        // aliases of each other) and `take_content_with_tools` must
        // clear the slot behind it (so a second `take` returns
        // `None`). Default must be all-`None` so
        // `LoopState::default()` semantics stay unchanged.
        let mut c = MessageCache::default();
        assert!(c.checkpoint().is_none());
        assert!(c.last_content_with_tools.is_none());

        let snapshot = vec![Message::user("checkpoint text")];
        c.store_checkpoint(snapshot.clone());
        c.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(c.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            c.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );

        // take_content_with_tools moves the value out and clears the
        // slot; checkpoint is untouched (take is per-slot).
        let taken = c.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(c.last_content_with_tools.is_none());
        assert!(c.checkpoint().is_some());

        // store_content_with_tools(None) is a no-op for callers that
        // want to clear without consuming (in case `None` means
        // "explicitly nothing" vs. the implicit `None` after a take).
        c.store_content_with_tools(None);
        assert!(c.last_content_with_tools.is_none());
    }

    #[test]
    pub(crate) fn p1_1_loop_state_round_trip() {
        // Phase 1.1 (2026-07-03): the bridge between `run()`'s 14
        // loop-local aliases and the 14 `state` fields. Publishing the
        // locals back into `state` is what makes the future
        // `run_shared_loop(state, llm_caller)` extraction possible
        // (the shared body only sees `&mut LoopState`).
        //
        // Phase 5 (2026-07-03): the `publish_from` bridge shrank from
        // 9 args to 5 args. The 4 cost-guard args (accumulated_usage /
        // session_cost_usd / cost_warned / forced_runtime_route) are
        // owned by `state.cost_guard` and are no longer bridged. The 3
        // retry-counter args and the 3 context-pressure args were
        // already removed in earlier Phase 5 sub-commits.
        //
        // Phase 5 (2026-07-04, 1st follow-up): shrank from 5 args to
        // 3 args. The 2 timing args (`total_api_time_ms` /
        // `total_tool_time_ms`) are owned by `state.timings` and
        // mutated in place by `record_api` / `record_tool`.
        //
        // Phase 5 (2026-07-04, 2nd follow-up): shrank from 3 args to
        // 1 arg. The 2 message-cache args
        // (`last_checkpoint_messages` / `last_content_with_tools`) are
        // owned by `state.message_cache` and mutated in place by
        // `store_checkpoint` / `store_content_with_tools`. We
        // exercise all 4 in-place paths in this test.
        //
        // Phase 5 (2026-07-04, 3rd follow-up): the `publish_from`
        // helper is removed entirely. The single remaining arg
        // (`total_turns`) is written directly to `state.total_turns`
        // in both `run()` and `run_stream()`. The bridge is dead.
        let mut s = LoopState::default();
        // total_turns — the only field still bridged, now assigned
        // directly without going through `publish_from`.
        s.total_turns = 7;
        assert_eq!(s.total_turns, 7);
        // timings live on the inner struct — exercise the in-place
        // record path that replaced the old `+= api_elapsed` locals.
        s.timings.record_api(1234);
        s.timings.record_tool(567);
        assert_eq!(s.timings.total_api_time_ms, 1234);
        assert_eq!(s.timings.total_tool_time_ms, 567);
        // message_cache lives on the inner struct — exercise the
        // store / take / checkpoint / content_with_tools paths that
        // replaced the old `last_*` local aliases.
        let snapshot = vec![Message::user("checkpoint text")];
        s.message_cache.store_checkpoint(snapshot.clone());
        s.message_cache.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(s.message_cache.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            s.message_cache.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );
        let taken = s.message_cache.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(s.message_cache.last_content_with_tools.is_none());
        // cost_guard defaults are unchanged by the bridge — owned by
        // the inner struct and mutated in place by the runtime.
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        // Phase 5b (2026-07-04): forced_runtime_route moved out of
        // cost_guard into the sibling route_state manager.
        assert!(s.route_state.forced_runtime_route.is_none());
        // retry_counters live on the inner struct — set directly here
        // to mirror pre-extraction semantics (the inner struct is
        // mutated in place by bump_* / reset_* methods).
        s.retry_counters.invalid_tool = 2;
        s.retry_counters.invalid_json = 3;
        s.retry_counters.truncated_tool = 1;
        assert_eq!(s.retry_counters.invalid_tool, 2);
        assert_eq!(s.retry_counters.invalid_json, 3);
        assert_eq!(s.retry_counters.truncated_tool, 1);
        // context_pressure is on the inner struct — set directly here to
        // mirror pre-extraction semantics (the inner struct is mutated
        // in place by should_emit_warning).
        s.context_pressure.warned_tier = 0.65;
        s.context_pressure.last_warn_percent = 0.71;
        assert_eq!(s.context_pressure.warned_tier, 0.65);
        assert_eq!(s.context_pressure.last_warn_percent, 0.71);
    }

    #[test]
    pub(crate) fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 30);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
    }

    // ── M1.6.5: build_session_summary regression tests ──
    //
    // These exercise the helper that produces the session-level
    // FocusConclusion written to memory.db at session end. Without
    // this, single-shot sessions (no delegate_task) leave memory.db
    // empty and cross-session recall finds nothing.

    pub(crate) fn msg_user(s: &str) -> Message {
        Message::user(s)
    }
    pub(crate) fn msg_assistant(s: &str) -> Message {
        Message::assistant(s)
    }

    #[test]
    pub(crate) fn build_session_summary_returns_none_without_user_turns() {
        let msgs = vec![msg_assistant("hi")];
        assert!(crate::memory_hooks::build_session_summary(&msgs).is_none());
    }

    #[test]
    pub(crate) fn build_session_summary_returns_none_for_empty_user_content() {
        let msgs = vec![msg_user("   ")];
        assert!(crate::memory_hooks::build_session_summary(&msgs).is_none());
    }

    #[test]
    pub(crate) fn build_session_summary_picks_last_user_and_assistant() {
        let msgs = vec![
            msg_user("first question"),
            msg_assistant("first answer"),
            msg_user("second question"),
            msg_assistant("second answer"),
        ];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        assert!(s.contains("second question"));
        assert!(s.contains("second answer"));
        assert!(!s.contains("first question"));
    }

    #[test]
    pub(crate) fn build_session_summary_handles_user_only() {
        let msgs = vec![msg_user("hello")];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        assert_eq!(s, "USER: hello");
    }

    #[test]
    pub(crate) fn build_session_summary_truncates_long_user_and_assistant() {
        let long_user: String = "x".repeat(500);
        let long_assistant: String = "y".repeat(1000);
        let msgs = vec![msg_user(&long_user), msg_assistant(&long_assistant)];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        // The 200-char user truncation produces 200 chars + "…" appended
        // to the prefix — assert no runaway growth.
        assert!(
            s.chars().count() < 700,
            "summary should be truncated, got {} chars",
            s.chars().count()
        );
        assert!(s.contains("…"), "long content should be truncated with …");
    }

    #[test]
    pub(crate) fn test_plugin_hooks_include_api_and_session_finalize_paths() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
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

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                let hook_types = [
                    HookType::PreApiRequest,
                    HookType::PostApiRequest,
                    HookType::OnSessionEnd,
                    HookType::OnSessionFinalize,
                    HookType::OnSessionReset,
                ];
                for hook in hook_types {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("/reset")], None))
            .expect("agent run should succeed");

        let seen = seen.lock().expect("seen lock");
        for required in [
            "pre_api_request",
            "post_api_request",
            "on_session_end",
            "on_session_finalize",
            "on_session_reset",
        ] {
            assert!(
                seen.iter().any(|name| name == required),
                "missing required hook: {required}; seen={:?}",
                *seen
            );
        }
    }

    #[test]
    pub(crate) fn test_plugin_hooks_include_api_paths_in_stream_mode() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct StreamingProvider;
        #[async_trait::async_trait]
        impl LlmProvider for StreamingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
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
                futures::stream::iter(vec![
                    Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("stream-done".to_string()),
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
                ])
                .boxed()
            }
        }

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks-stream".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture stream hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                for hook in [HookType::PreApiRequest, HookType::PostApiRequest] {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StreamingProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run_stream(
                vec![Message::user("stream hello")],
                None,
                Some(Box::new(|_| {})),
            ))
            .expect("stream run should succeed");

        let seen = seen.lock().expect("seen lock");
        assert!(seen.iter().any(|name| name == "pre_api_request"));
        assert!(seen.iter().any(|name| name == "post_api_request"));
    }

    #[test]
    pub(crate) fn test_llm_request_model_strips_provider_prefix() {
        use futures::stream::BoxStream;

        struct CaptureModelProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CaptureModelProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen_model lock") = model.map(|m| m.to_string());
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
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

        let seen_model = Arc::new(std::sync::Mutex::new(None));
        let provider = CaptureModelProvider {
            seen_model: seen_model.clone(),
        };
        let config = AgentConfig {
            model: "openrouter:deepseek/deepseek-v3.2".to_string(),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(provider));

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let seen = seen_model.lock().expect("seen_model lock");
        assert_eq!(seen.as_deref(), Some("deepseek/deepseek-v3.2"));
    }

    /// 2026-06-29: end-to-end check that
    /// `auto_compress_if_over_threshold` emits at least one
    /// `compaction_status` snapshot (pre- and post-attempt) and that
    /// the snapshot carries the live pressure numbers. Mirrors the
    /// `status_callback_receives_context_pressure_messages` test
    /// above — that one covered the *string* `lifecycle` event, this
    /// one covers the *structured* `BreakerSnapshot` event the
    /// `CompactionStatusBanner` consumes.
    #[tokio::test]
    pub(crate) async fn compaction_status_callback_receives_snapshots() {
        use crate::compression::{CompressorConfig, ContextCompressor};
        use futures::stream::BoxStream;
        use hermix_intelligence::auxiliary::{
            AuxiliaryClient, AuxiliarySource, ProviderCandidate,
        };
        use std::sync::Arc;

        struct LocalDummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for LocalDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        // Capture every snapshot the callback sees — the test
        // asserts both the pressure-detected pre-snapshot and the
        // post-attempt snapshot arrive with non-zero pressure.
        let captured: Arc<std::sync::Mutex<Vec<crate::compression::BreakerSnapshot>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            on_compaction_status: Some(Arc::new(move |snap| {
                cap_ref
                    .lock()
                    .expect("compaction capture lock")
                    .push(snap.clone());
            })),
            ..Default::default()
        };

        // Build a minimal compressor with an in-process auxiliary
        // provider so `compress()` can produce a real summary.
        // Pattern mirrored from `compression::tests::aux_with_provider`.
        let aux = Arc::new(
            AuxiliaryClient::builder()
                .add_candidate(ProviderCandidate::new(
                    AuxiliarySource::Custom,
                    "test-model",
                    Arc::new(LocalDummyProvider) as Arc<dyn LlmProvider>,
                ))
                .build(),
        );
        let mut cfg = CompressorConfig::default();
        cfg.context_length = 1000;
        cfg.threshold_percent = 0.8;
        cfg.quiet_mode = true;
        let compressor = ContextCompressor::new(cfg, aux);

        // The agent LLM and the compressor's auxiliary client hold
        // independent provider chains. `LocalDummyProvider` is a zero
        // sized struct with empty state, so a fresh `Arc::new` per
        // use site costs nothing and shares nothing.
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(LocalDummyProvider),
        )
        .with_callbacks(callbacks);
        agent.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));

        // Force the context over threshold. `ContextManager::new(100)`
        // sets max=100; we zero the output reserve so the pressure
        // threshold is the documented 80 tokens. The user message
        // needs to tokenise to > 80 — `estimate_tokens` divides by
        // 4, so 400 chars yields 100 tokens, comfortably above.
        //
        // `compress()` requires `n_messages > protect_first_n + 4`
        // (7 messages by default), so we add several user+assistant
        // pairs to give the compressor real middle-turns to summarise.
        let mut ctx = ContextManager::new(100).with_output_reserve(0);
        for i in 0..12 {
            ctx.add_message(Message::user(format!("user {} {}", i, "x".repeat(80))));
            ctx.add_message(Message::assistant(format!("assistant {} {}", i, "y".repeat(80))));
        }
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let snaps = captured.lock().expect("snaps lock");
        assert!(
            snaps.len() >= 2,
            "expected pressure + post-attempt snapshots, got {}",
            snaps.len()
        );
        // Pre-snapshot must carry the live pressure so the banner
        // can render "x / 80 tokens" before any LLM work.
        assert!(snaps[0].used_tokens > 0, "pre-snap used_tokens=0");
        assert_eq!(snaps[0].threshold_tokens, 50, "pre-snap threshold");
        // Post-snapshot — state stays Closed (no failures).
        // `compression_count` is always 0 because the engine-agnostic
        // snapshot bridge (`CompressionEngineSnapshot` → `BreakerSnapshot`)
        // defaults it. What we CAN assert: state is Closed and the
        // threshold is populated.
        let last = snaps.last().expect("at least one snap");
        assert_eq!(last.state, crate::compression::BreakerState::Closed);
        assert!(last.used_tokens > 0, "post-snap used_tokens=0");
        assert_eq!(last.threshold_tokens, 50, "post-snap threshold");
    }

    #[test]
    pub(crate) fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    pub(crate) fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
                Some("memory"),
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
                Some("skill"),
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}", Some("skip")),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[tokio::test]
    pub(crate) async fn status_callback_receives_context_pressure_messages() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    #[tokio::test]
    pub(crate) async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    pub(crate) async fn stream_collect_emits_activity_while_waiting_for_chunks() {
        use async_stream::stream;
        use futures::stream::BoxStream;

        struct DelayedStreamProvider;

        #[async_trait::async_trait]
        impl LlmProvider for DelayedStreamProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("noop"),
                    usage: None,
                    model: "dummy".into(),
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
                Box::pin(stream! {
                    tokio::time::sleep(Duration::from_millis(3500)).await;
                    yield Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("hi".into()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                    yield Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".into()),
                        usage: None,
                    });
                })
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            stream_activity_interval_secs: 1,
            stream_activity_stall_secs: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DelayedStreamProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(10_000);
        ctx.add_message(Message::user("ping"));

        let noop: &(dyn Fn(StreamChunk) + Send + Sync) = &|_chunk| {};
        let outcome = agent
            .collect_stream_llm_response(&ctx, &[], None, "openai:gpt-4o", noop)
            .await
            .expect("stream collect");

        assert!(matches!(outcome, StreamCollectOutcome::Complete(_)));
        let rows = captured.lock().expect("captured lock");
        let warm = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("Still waiting"));
        let stall = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("may be stuck"));
        assert!(
            warm && stall,
            "expected one warm + one stall activity; got {:?}",
            *rows
        );
    }

    #[tokio::test]
    pub(crate) async fn quiet_mode_suppresses_status_callback() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    pub(crate) fn test_builtin_personality_injected_into_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    pub(crate) fn test_unknown_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
    }

    #[test]
    pub(crate) fn test_formatting_discipline_guidance_in_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        // No personality, no tools, no plugin context — formatting discipline
        // and thinking discipline must still be present in every prompt.
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            prompt.contains("Output formatting discipline"),
            "formatting discipline block missing from system prompt"
        );
        assert!(prompt.contains("DO NOT"));
        assert!(prompt.contains("Code identifiers"));
        assert!(
            prompt.contains("Output format — strict"),
            "output format contract block missing from system prompt"
        );
        assert!(prompt.contains("Hard rules"));
        assert!(prompt.contains("Correct example"));
        assert!(prompt.contains("Wrong example"));
    }

    #[test]
    pub(crate) fn test_active_workspace_block_injected_when_set() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        // No workspace → no block.
        let no_ws_agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&no_ws_agent.config, &no_ws_agent.prompt_cache, no_ws_agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            !prompt.contains("## Active Workspace"),
            "workspace block should be absent when no workspace is configured"
        );

        // With workspace → block surfaces the path verbatim.
        let mut config = AgentConfig::default();
        config.workspace = Some("/work/qcc".to_string());
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            prompt.contains("## Active Workspace"),
            "workspace block missing"
        );
        assert!(
            prompt.contains("/work/qcc"),
            "workspace path missing from block"
        );
    }

    #[test]
    pub(crate) fn test_tool_usage_guidance_in_system_prompt() {
        // TOOL_USAGE_GUIDANCE must be present in every prompt (any model, any toolset)
        // because the "use a generic tool when a purpose-built one exists" failure
        // mode is universal. Don't gate it on tool_enforcement model list.
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // 1. Guidance present for any model (even one not in the tool-enforcement list).
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "unknown-model-xyz", 0);
        assert!(
            prompt.contains("Tool selection discipline"),
            "TOOL_USAGE_GUIDANCE block must be unconditional, not gated on model"
        );
        assert!(
            prompt.contains("scan the available tool list"),
            "decision pattern step 1 (scan tools) missing"
        );
        assert!(
            prompt.contains("call it first"),
            "decision pattern step 2 (use purpose-built tool first) missing"
        );
        assert!(
            prompt.contains("Anti-patterns"),
            "anti-pattern block missing"
        );

        // 2. MiniMax models must now be in the tool-enforcement whitelist so they
        // get the "must use tools" guidance in addition to the selection discipline.
        assert!(
            crate::prompt_assembly::should_inject_tool_enforcement("minimax-m2.7"),
            "MiniMax M2.7 should be in tool-enforcement whitelist"
        );
        assert!(
            crate::prompt_assembly::should_inject_tool_enforcement("M2-highspeed"),
            "M2 variant should be in tool-enforcement whitelist"
        );
        // Spot-check existing whitelist still works.
        assert!(crate::prompt_assembly::should_inject_tool_enforcement("gpt-4o"));
        assert!(!crate::prompt_assembly::should_inject_tool_enforcement("claude-sonnet-4"));
    }

    #[test]
    pub(crate) fn test_smart_model_routing_cheap_route_for_simple_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("帮我总结一下今天要做什么")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    pub(crate) fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = crate::cost_routing::primary_runtime_snapshot(
            &agent.config,
            agent.primary_credential_pool.as_ref(),
        );
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-oauth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermix_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    pub(crate) fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    pub(crate) fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
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

        // Fixture-style cases distilled from Python v2026.4.16:
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = agent.evolution_counters.lock().expect("counter lock");
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    pub(crate) fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermix_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermix_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
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

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    pub(crate) fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp://copilot".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_skips_complex_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert!(selected.is_none());
    }

    #[test]
    pub(crate) fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "2".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "3".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    pub(crate) fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
        };
        let event = crate::memory_hooks::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    pub(crate) fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
        };
        let event = crate::memory_hooks::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    pub(crate) fn test_hydrate_session_search_args_injects_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    pub(crate) fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    pub(crate) fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
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

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        let max = agent.config.max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    pub(crate) fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    pub(crate) fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost: Some(0.01),
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: Some(0.02),
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    pub(crate) fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: None,
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    pub(crate) fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    pub(crate) fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            estimated_cost: None,
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

    // ── ContextPressureMonitor unit tests (2026-07-03) ──
    //
    // The helper is the gate that decides whether `run()` / `run_stream()`
    // should fire a `lifecycle` warning when the context window fills up.
    // It had no direct unit tests — coverage only came through the
    // integration path. These tests exercise the three decision rules
    // (tier upgrade, cooldown, percent step) using deterministic state
    // and rely on the helper's own `Instant::now()` for time, so no
    // wall-clock waiting is needed.

    #[test]
    pub(crate) fn cp_monitor_default_is_all_zero_and_none() {
        // Phase 5: the freshly-extracted ContextPressureMonitor must
        // behave like the old 3 inline fields when not yet primed —
        // otherwise the first call inside run() / run_stream() would
        // see stale defaults.
        let m = ContextPressureMonitor::default();
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn cp_monitor_returns_false_when_tier_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.42, 0.0);
        assert!(!emit);
        // No state mutation when no emission.
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn cp_monitor_emits_on_first_call_above_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.65, 0.65);
        assert!(emit);
        // Tier upgrade always emits and records tier + warn_at + percent.
        assert_eq!(m.warned_tier, 0.65);
        assert!(m.last_warn_at.is_some());
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    pub(crate) fn cp_monitor_blocks_within_cooldown_without_tier_upgrade() {
        let mut m = ContextPressureMonitor::default();
        // Seed: same tier, recent emit at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Same tier, <5% advance, immediately after prior emit → cooldown blocks.
        let emit = m.should_emit_warning(0.66, 0.65);
        assert!(!emit);
        // State unchanged.
        assert_eq!(m.warned_tier, 0.65);
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    pub(crate) fn cp_monitor_emits_on_tier_upgrade_even_within_cooldown() {
        let mut m = ContextPressureMonitor::default();
        // Seed: tier=0.65, just warned at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Tier upgrade overrides the cooldown.
        let emit = m.should_emit_warning(0.66, 0.85);
        assert!(emit);
        assert_eq!(m.warned_tier, 0.85);
        assert!(m.last_warn_at.is_some());
    }

    // ── Phase 5 (2026-07-03): RetryCounters unit tests ──
    //
    // The 3 sibling retry counters on LoopState (invalid_tool_retries,
    // invalid_json_retries, truncated_tool_call_retries) collapsed into
    // a single RetryCounters struct. These tests pin the bump + reset
    // contract; the `< max` / `>= max` decision stays at the call site
    // because the action on max differs between the three.

    #[test]
    pub(crate) fn retry_counters_default_is_all_zero() {
        // Phase 5: the freshly-constructed RetryCounters must start at
        // zero for all three slots — otherwise a stale value would leak
        // into the first call inside run() / run_stream().
        let c = RetryCounters::default();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    pub(crate) fn retry_counters_bump_returns_saturating_incremented_value() {
        // All three bump methods return the new value and saturate
        // instead of overflowing — required because a buggy provider
        // could otherwise pin a counter at u32::MAX forever.
        let mut c = RetryCounters::default();
        assert_eq!(c.bump_invalid_tool(), 1);
        assert_eq!(c.bump_invalid_tool(), 2);
        assert_eq!(c.bump_invalid_json(), 1);
        assert_eq!(c.bump_invalid_json(), 2);
        assert_eq!(c.bump_truncated_tool(), 1);
        // Saturating behavior: simulate pathological case.
        c.invalid_tool = u32::MAX;
        assert_eq!(c.bump_invalid_tool(), u32::MAX);
        // Other counters untouched by the saturating bump.
        assert_eq!(c.invalid_json, 2);
        assert_eq!(c.truncated_tool, 1);
    }

    #[test]
    pub(crate) fn retry_counters_reset_clears_only_the_targeted_slot() {
        // The three reset methods are independent — resetting one
        // counter must not touch the other two, otherwise a successful
        // turn (which resets invalid_tool + invalid_json) could
        // accidentally wipe the streaming-only truncated_tool counter.
        let mut c = RetryCounters::default();
        c.bump_invalid_tool();
        c.bump_invalid_tool();
        c.bump_invalid_json();
        c.bump_truncated_tool();
        c.reset_invalid_tool();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 1);
        assert_eq!(c.truncated_tool, 1);
        c.reset_invalid_json();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 1);
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    pub(crate) fn retry_counters_truncated_tool_bump_at_max_still_increments() {
        // The truncated_tool call site checks `< max` BEFORE bumping
        // (run_stream's "Truncated tool arguments — retrying" branch
        // only fires when under the cap). After the bump-method
        // refactor, the bump happens unconditionally and the call
        // site does the `<= max` check after. The end-state observable
        // behavior is the same: at the cap, the bump-then-check path
        // is a no-op for the emit, and the next reset() wipes the
        // counter. This test pins the bumped value so a future
        // refactor that introduces an early-return would be caught.
        let mut c = RetryCounters::default();
        c.truncated_tool = 2; // simulate "just under the cap of 3"
        assert_eq!(c.bump_truncated_tool(), 3); // bumps to 3
        assert_eq!(c.bump_truncated_tool(), 4); // would be 4 (over the cap)
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    // ── Phase 5 (2026-07-03): CostGuard struct + evaluate_thresholds ──
    //
    // The 4 cost-guard fields that used to be 4 sibling `&mut` args on
    // `process_post_response` now live on a single `CostGuard` struct.
    // `evaluate_thresholds` returns an enum (NoAction / Downgrade /
    // Warn / Trip) so the caller — not the helper — owns the side
    // effects (route build + system message).

    #[test]
    pub(crate) fn cost_guard_default_is_all_zeros_and_none() {
        let cg = CostGuard::default();
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
        assert!(!cg.cost_warned);
        // Phase 5b (2026-07-04): forced_runtime_route moved out
        // (route_state owns it now).
    }

    #[test]
    pub(crate) fn cost_guard_record_usage_merges_into_accumulated_and_adds_cost() {
        let mut cg = CostGuard::default();
        // First usage: 10 prompt + 20 completion → starts a new total.
        cg.record_usage(
            Some(&UsageStats {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                estimated_cost: Some(0.001),
            }),
            "test-model",
            &AgentConfig::default(),
        );
        assert!(cg.accumulated_usage.is_some());
        assert!(cg.session_cost_usd >= 0.0);
    }

    #[test]
    pub(crate) fn cost_guard_record_usage_no_op_when_turn_usage_is_none() {
        let mut cg = CostGuard::default();
        cg.record_usage(None, "test-model", &AgentConfig::default());
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_no_action_when_no_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 999.0;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, None, 0.5, || None);
        assert_eq!(action, CostGuardAction { warn: None, trip: None });
        assert!(!cg.cost_warned);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_downgrade_when_warn_threshold_first_crossed() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06; // above 0.1 * 0.5
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("openai:gpt-4o-mini".into()));
        match action.warn {
            Some(WarnEffect { kind: WarnKind::Downgrade { model }, .. }) => {
                assert_eq!(model, "openai:gpt-4o-mini");
            }
            other => panic!("expected Downgrade warn effect, got {:?}", other),
        }
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
        // forced_runtime_route is NOT set by evaluate_thresholds —
        // the caller must call `RouteState::install_downgrade` with
        // the built `TurnRuntimeRoute`. This is the whole reason the
        // helper doesn't construct the route itself.
        assert!(rs.forced_runtime_route.is_none());
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_warn_when_degrade_model_unresolvable() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || None);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Warn, .. })
        ));
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_is_idempotent_after_first_warn() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        let mut rs = RouteState::default();
        // First crossing fires the downgrade side effect.
        let first = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("cheap".into()));
        assert!(matches!(
            first.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
        // Caller installs the route (mirrors what process_post_response
        // does in production). We only need *some* route to verify the
        // forced_runtime_route field gets populated; a partial struct
        // literal is fine.
        rs.install_downgrade(TurnRuntimeRoute {
            model: "cheap".into(),
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: None,
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("cost_guard".into()),
            signature: TurnRouteSignature {
                model: "cheap".into(),
                provider: None,
                base_url: None,
                api_mode: ApiMode::ChatCompletions,
                command: None,
                args: Vec::new(),
            },
        });
        assert!(rs.forced_runtime_route.is_some());
        // Second evaluation: already past warn, no degrade, not yet
        // over the absolute limit → no warn + no trip.
        let second = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("another".into()));
        assert_eq!(second, CostGuardAction { warn: None, trip: None });
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_trip_when_spend_exceeds_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.5;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("cheap".into()));
        let trip = action
            .trip
            .expect("expected trip effect, got None");
        assert!((trip.session_cost_usd - 0.5).abs() < 1e-9);
        assert!((trip.limit - 0.1).abs() < 1e-9);
        // The trip path also flipped cost_warned on the way through
        // (since 0.5 > 0.1 * 0.5 = 0.05). Both warn AND trip fired —
        // caller fires both side effects in order.
        assert!(cg.cost_warned);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
    }

    // ── Phase 1.3a (2026-07-03): process_post_response unit tests ──
    //
    // The shared post-LLM helper extracted in 1.3a replaces ~200 lines
    // of duplication between `run()` and `run_stream()`. These tests
    // exercise the two return paths (Finalize / Continue) and the
    // usage-accumulation side effect.

    #[test]
    pub(crate) fn p1_3a_process_post_response_finalize_on_no_tool_calls() {
        // No tool calls in response → Finalize. The helper should also
        // copy the assistant visible text into `last_content_with_tools`
        // for ack-continuation tracking.
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("final answer"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        // Phase 5 (2026-07-04): `&mut Option<String>` arg became
        // `&mut MessageCache`; the old `last_content` local is gone,
        // we exercise the same final-answer path through
        // `message_cache.last_content_with_tools`.
        // Phase 5b (2026-07-04): added `&mut RouteState` — the
        // forced-route state used to live on `CostGuard`.
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0, // total_turns
                0, // api_elapsed
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false, // stream_mode
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Finalize));
        assert_eq!(message_cache.last_content_with_tools.as_deref(), Some("final answer"));
    }

    #[test]
    pub(crate) fn p1_3a_process_post_response_cost_guard_trips_when_limit_exceeded() {
        // `session_cost_usd` already over `max_cost_usd` → CostGuardTripped
        // (caller should return finalize with finished_naturally=false).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let mut cfg = AgentConfig::default();
        cfg.max_cost_usd = Some(0.01);
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("anything"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        cost_guard.session_cost_usd = 0.5; // already 50x over the 0.01 limit
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0,
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert_eq!(action, PostResponseAction::CostGuardTripped);
    }

    #[test]
    pub(crate) fn p1_3a_process_post_response_continue_on_tool_calls() {
        // Tool calls in response → Continue. The helper should leave
        // `last_content_with_tools` untouched (caller handles that).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let mut msg = Message::assistant("calling tool");
        msg.tool_calls = Some(vec![ToolCall {
            id: "tc-1".into(),
            function: hermix_core::FunctionCall {
                name: "echo".into(),
                arguments: "{}".into(),
            },
        }]);
        let response = hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "test".into(),
            finish_reason: Some("tool_calls".into()),
        };
        let mut cost_guard = CostGuard::default();
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0, // api_elapsed
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Continue));
        // last_content_with_tools untouched (set later by the loop after
        // the assistant message is appended to ctx).
        assert!(message_cache.last_content_with_tools.is_none());
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_allow_when_hook_unset() {
        // No hook installed → resolve_pre_dispatch returns an empty map and
        // never blocks.
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        );
        let tcs = vec![ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        }];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert!(decisions.is_empty());
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_routes_through_hook() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(move |tc: &ToolCall| {
                calls_clone.fetch_add(1, Ordering::SeqCst);
                if tc.function.name == "dangerous_tool" {
                    ToolDispatchDecision::Deny {
                        reason: "blocked by test".to_string(),
                    }
                } else {
                    ToolDispatchDecision::Allow
                }
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![
            ToolCall {
                id: "a".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "b".into(),
                function: hermix_core::FunctionCall {
                    name: "dangerous_tool".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(decisions.get("a").unwrap().is_ok());
        assert_eq!(
            decisions.get("b").unwrap().as_ref().err().unwrap(),
            "blocked by test"
        );
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_await_approval_blocks_then_resolves() {
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(|_tc: &ToolCall| {
                let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
                ToolDispatchDecision::AwaitApproval(rx)
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![ToolCall {
            id: "x".into(),
            function: hermix_core::FunctionCall {
                name: "x".into(),
                arguments: "{}".into(),
            },
        }];
        // Drop the spawned sender right away → receiver yields Err, hook
        // converts that into "Approval channel closed" denial.
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        let err = decisions.get("x").unwrap().as_ref().err().unwrap();
        assert!(err.contains("closed"), "unexpected err: {err}");
    }

    // -----------------------------------------------------------------------
    // M1.6.2: push/pop pair — `note_delegation_start` is wired into the
    // tool-dispatch paths and `note_delegation_end` matches by empty id
    // (topmost Delegation). Verify the pair generates exactly one
    // FocusConclusion per delegation.
    // -----------------------------------------------------------------------

    #[test]
    pub(crate) fn m162_delegation_push_pop_pair_emits_one_conclusion() {
        use hermix_focus::{FocusStack, SessionFrame, SessionFrameStatus};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // session root
        agent.note_session_started("parent turn");
        // push with empty sub_agent_id (matches dispatch path behaviour
        // — the real sub_agent_id is parsed later from tool result content)
        let fid = agent
            .note_delegation_start("sub-task A", "")
            .expect("push succeeds");
        assert!(fid.is_nil() == false, "frame id is non-nil");

        // Stack depth should be 2 (root + delegation).
        let stack_depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth, 2, "expected root + delegation");

        // Pop with empty id → matches topmost Delegation.
        let popped_depth = agent
            .note_delegation_end("", SessionFrameStatus::Completed)
            .expect("pop succeeds");
        // `note_delegation_end` returns the frame's *payload* depth
        // field (delegate_depth counter, not stack depth). The default
        // agent has delegate_depth=0, so the popped frame's depth is 0.
        // Stack depth assertion happens below.
        assert_eq!(popped_depth, 0, "popped frame's depth field");

        // Stack should now have 1 frame (just root).
        let stack_depth_after = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth_after, 1);

        // Drain conclusions — expect exactly one.
        // (drain_focus_conclusions_to_memory_router returns 0 when
        // memory_router is None, so check the focus stack directly.)
        let drained = agent
            .focus_stack
            .lock()
            .expect("lock")
            .drain_conclusions()
            .len();
        assert_eq!(drained, 1, "push/pop pair should emit exactly one conclusion");
    }

    #[test]
    pub(crate) fn m162_push_idempotent_when_session_root_already_present() {
        use hermix_focus::{FocusStack, SessionFrame};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // Calling note_session_started twice should be idempotent (no duplicate root).
        agent.note_session_started("first");
        agent.note_session_started("second");
        let depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(depth, 1, "idempotent — only one root frame");
    }

    #[tokio::test]
    pub(crate) async fn drain_focus_conclusions_persists_to_memory_router() {
        use hermix_focus::{EmbeddedMemoryBackend, FocusStack, MemoryRouter, SessionFrame};
        use std::sync::Mutex;

        // Create a temporary embedded backend
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let embedded = Arc::new(
            EmbeddedMemoryBackend::open(&db_path).expect("open embedded backend"),
        );
        let router = Arc::new(MemoryRouter::new_with_embedded(
            vec![embedded.clone()],
            embedded.clone(),
        ));

        // Create agent with focus stack and memory router
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())))
        .with_memory_router(router);

        // Push a delegation frame and pop it to generate a conclusion
        agent.note_session_started("test-session");
        agent.note_delegation_start("test task summary", "sub-agent-1");
        agent.note_delegation_end("sub-agent-1", hermix_focus::SessionFrameStatus::Completed);

        // Drain conclusions
        let count = agent.drain_focus_conclusions_to_memory_router();
        assert!(count >= 1, "should drain at least one conclusion");

        // Wait for async write
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify the conclusion was persisted - search for conclusion tag
        let query = hermix_focus::MemoryQuery::recall("conclusion");
        let (hits, _) = embedded.recall(&query).await.unwrap();
        assert!(!hits.is_empty(), "conclusion should be retrievable from embedded backend");
    }

    // -- M5 ACUI emit tests ---------------------------------------------

    /// Helper: build a minimal agent with no callbacks wired.
    pub(crate) fn bare_agent() -> AgentLoop {
        AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
    }

    #[test]
    pub(crate) fn m5_emit_acui_no_callback_is_noop() {
        // Without `on_acui_directive` wired, emit_acui should silently drop.
        let agent = bare_agent();
        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        agent.emit_acui_update("c1", serde_json::json!({"temp": 23}));
        agent.emit_acui_hide("c1");
        // No panic, no return value to assert.
    }

    #[test]
    pub(crate) fn m5_emit_acui_show_invokes_callback_with_json() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        // We own the only Arc to `agent.callbacks` here, so get_mut
        // succeeds and we can install the callback directly.
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "show");
        assert_eq!(v["type"], "weather");
        assert_eq!(v["id"], "c1");
        assert_eq!(v["props"]["city"], "Beijing");
        assert_eq!(v["position"], "after");
    }

    #[test]
    pub(crate) fn m5_emit_acui_update_serializes_id_and_patch() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_update("card-7", serde_json::json!({"step": 5}));
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "update");
        assert_eq!(v["id"], "card-7");
        assert_eq!(v["patch"]["step"], 5);
    }

    #[test]
    pub(crate) fn m5_emit_acui_hide_serializes_id() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_hide("c1");
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "hide");
        assert_eq!(v["id"], "c1");
    }
}

/// Lightweight stub provider for `resolve_pre_dispatch` tests. Implements the
/// minimum `LlmProvider` surface so we can construct an `AgentLoop` without
/// pulling a real provider into a test.
#[cfg(test)]
mod stub_provider {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};

    pub struct EmptyProvider;

    #[async_trait]
    impl LlmProvider for EmptyProvider {
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
                message: Message::assistant(""),
                usage: None,
                model: "stub".into(),
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

    pub(crate) fn make_reasoning_response(reasoning: &str) -> hermix_core::LlmResponse {
        let mut msg = Message::assistant("");
        msg.reasoning_content = Some(reasoning.to_string());
        hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    pub(crate) fn make_empty_response() -> hermix_core::LlmResponse {
        hermix_core::LlmResponse {
            message: Message::assistant(""),
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    pub(crate) fn make_loop_for_classify() -> (AgentLoop, ContextManager) {
        let config = AgentConfig::default();
        // `classify_post_llm_response` is a pure synchronous classifier
        // — it never touches the LLM provider, tool registry, or any
        // IO channel — so an empty stub provider and an empty registry
        // are enough to satisfy the `AgentLoop::new` constructor.
        let loop_ = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(EmptyProvider),
        );
        let ctx = ContextManager::new(100);
        (loop_, ctx)
    }

    #[test]
    pub(crate) fn p4_1_classify_breaks_on_tool_calls() {
        // Tool calls present → Break(r) regardless of content /
        // reasoning state. Caller unwraps the response and dispatches.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let mut msg = Message::assistant("");
        msg.tool_calls = Some(vec![hermix_core::ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "noop".into(),
                arguments: "{}".into(),
            },
        }]);
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: msg,
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    pub(crate) fn p4_1_classify_breaks_on_visible_text() {
        // No tools, but assistant content is non-empty → Break(r).
        // Counter budgets must not be touched.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: Message::assistant("hello world"),
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    pub(crate) fn p4_1_classify_retries_reasoning_within_budget() {
        // Reasoning-only response with the budget set to 2 → first
        // call increments to 1/2 and returns RetryReasoning; the
        // message is pushed onto ctx so the next LLM call sees it as
        // prior context.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.thinking_prefill_max_retries = 2;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_reasoning_response("thinking…"),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::RetryReasoning));
        assert_eq!(inner_thinking, 1);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 1);
    }

    #[test]
    pub(crate) fn p4_1_classify_retries_empty_within_budget() {
        // No tools, no reasoning, no content → RetryEmpty (not
        // RetryReasoning). The message must NOT be pushed — empty
        // responses are noise that shouldn't pollute the conversation.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.empty_content_max_retries = 1;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_empty_response(),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::RetryEmpty));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 1);
        assert_eq!(ctx.get_messages().len(), 0);
    }
}
