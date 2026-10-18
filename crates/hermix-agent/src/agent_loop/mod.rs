//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermix_focus::{FocusStack, SessionFrame, SessionFrameStatus};
#[cfg(test)]
use hermix_focus::MemoryLayer;
// `chrono` (DateTime/Utc) dropped on when `OAuthStoreCredential`
// moved to `crate::oauth_refresh`.
// serde::Serialize / serde::Deserialize were dropped on when
// `OAuthStoreCredential` moved to `crate::oauth_refresh`; this file no
// longer holds any #[derive(Serialize, Deserialize)] structs.
#[cfg(test)]
use futures::StreamExt;
use serde_json::Value;
#[cfg(test)]
use std::time::{Duration, Instant};

use hermix_core::{
    AcuiDirective, AgentError, AgentResult, CompressionEngine, LlmProvider, Message,
    MessageRole, StreamChunk, ToolCall, ToolResult, ToolSchema, UsageStats,
};
use hermix_intelligence::{
    prefetch::Prefetcher,
    ToolSchemaRouter, TurnContext,
};

use crate::archive::ArchiveRef;
use crate::context::ContextManager;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::plugins::{HookResult, HookType, PluginManager};
#[cfg(test)]
use crate::python_alignment::budget_pressure_text;
use crate::python_alignment::{
    sanitize_surrogates, strip_budget_warnings_from_messages,
};
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
#[cfg(test)]
pub(crate) use crate::cost_guard::WarnKind;

// ---------------------------------------------------------------------------
//Backwards-compat re-exports (P2.2.4, )
// ---------------------------------------------------------------------------
// `AgentConfig`, `RetryConfig`, and `RuntimeProviderConfig` moved to
// `crate::agent_config` on (REFACTOR_PLAN §4). Two test files
// still import via `hermix_agent::agent_loop::AgentConfig`, so we keep
// these `pub use` aliases live — same source-of-truth, just a thinner
// alias to the new module.
// `ToolEntry`, `ToolRegistry`, `TurnMetrics`, `EvolutionCounters`,
// `ToolDispatchDecision`, `AgentCallbacks`, `ErrorClass`, the helper
// functions, and `TOOL_RESULT_CHAR_LIMIT` were extracted to
// `crate::agent_helpers` on (P2.2.5). Same source-of-truth,
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
//OAuth credential + refresh manager (extracted )
// ---------------------------------------------------------------------------
// `OAuthRefresh` + `OAuthStoreCredential` moved to `crate::oauth_refresh`.
// Kept as `pub use` aliases so `hermix_agent::agent_loop::OAuthRefresh`
// remains a valid import path for any external consumer (e.g. hermix-cron,
// test files). The implementation now lives next to the HTTP code it
// orchestrates (reqwest POST, on-disk JSON store, per-provider backoff)
// — see `oauth_refresh.rs` for the full surface and unit tests.

pub use crate::oauth_refresh::{OAuthRefresh, OAuthStoreCredential};

pub(crate) use constants::*;
mod constants;

use crate::stream_collect::StreamCollectOutcome;

pub(crate) use types::*;
mod types;

// ---------------------------------------------------------------------------
/// The main agent loop.
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
/// ## Lock ordering
/// When acquiring multiple locks, always follow this order to prevent deadlocks:
/// 1. `tool_registry` (RwLock)
/// 2. `plugin_manager` (Mutex)
/// 3. `focus_stack` (Mutex)
/// 4. `compression` (Mutex)
/// 5. Other Mutex fields (evolution_counters, tool_error_counter, etc.)
/// Never hold a lock from a later tier while calling code that acquires
/// an earlier-tier lock.
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
    /// per-tool error counter, drained from every
    /// `finalize_agent_result` call so the agent loop can see its own
    /// repeated-failure pattern and demote tools via the
    /// `[Tool Failure Hints]` block. Lives on the same `AgentLoop`
    /// so it survives across `run()` / `run_stream()` calls.
    tool_error_counter: Arc<Mutex<crate::tool_error_demotion::ToolErrorCounter>>,
    /// Manager for runtime-provider OAuth token refresh + backoff state.
    pub oauth_refresh: OAuthRefresh,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    /// `pub(crate)` so `tool_executor.rs` can read it from the moved
    /// `execute_tool_calls` method . The field stays
    /// crate-private — external callers must use the agent builder API.
    pub(crate) sub_agent_orchestrator: Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Mid-run `/steer` buffer (Python `AIAgent._pending_steer`).
    pending_steer: Arc<Mutex<Option<String>>>,
    /// Last emitted [`LoopPhase`], packed as `u8` (`0` = none yet).
    /// Used by `notify_phase` to flag illegal transitions — the phase
    /// sequence is a contract (`PostLlmCall → ToolValidation → …`), and a
    /// silent skip is how a bad branch turns into "the turn just stopped".
    /// `AtomicU8` keeps `notify_phase` `&self` and lock-free.
    pub(crate) last_loop_phase: std::sync::atomic::AtomicU8,
    /// Optional tool schema router. When set, the LLM only sees the routed
    /// subset of `tool_registry` ( H1 — token saving, see
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3). When `None`, the full
    /// registry is sent (legacy behavior).
    pub tool_router: Option<Arc<dyn ToolSchemaRouter>>,
    /// Time-aware recall. When set, `memory_prefetch`
    /// extracts a `time_window` from the user query, fans out to all
    /// `MemoryLayer` backends through the router, deduplicates and
    /// re-ranks hits by salience, and injects the resulting structured
    /// blocks as a `## Memory` section of the system prompt. This is
    /// the **only** read path; the legacy `MemoryManager::prefetch_all`
    /// markdown path was removed in .
    /// the **write** path is also wired through this
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
    /// H2: Intent-aware prefetcher. When set, the agent loop
    /// classifies the user message into a `PrefetchIntent` *before*
    /// the LLM call, dispatches read-only (Safe) tools in parallel,
    /// and injects results as a system-prompt block. This reduces
    /// the time-to-first-token by overlapping tool execution with
    /// LLM reasoning.
    pub prefetcher: Option<Arc<Prefetcher>>,
    /// Learned prefetch stats (persisted across turns/sessions).
    learned_prefetch_stats: Arc<Mutex<hermix_intelligence::prefetch::IntentStats>>,
    /// per-session `FocusStack<SessionFrame>`. The agent loop
    /// pushes a frame on every `delegate_task` invocation and pops
    /// it on completion; a `FocusConclusion` is auto-generated and
    /// fed to the memory router for long-term indexing. See
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 + .
    /// The stack starts empty; callers push the session root
    /// (one frame per session) at `run()` start via
    /// `note_session_started`, and sub-agents push/pop via
    /// `note_delegation_start` / `note_delegation_end`.
    pub focus_stack: Arc<Mutex<FocusStack<SessionFrame>>>,
    /// optional consciousness tracker for broadcasting
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
    /// (A): raw `turns_to_summarize` from the most recent
    /// compression, captured by `ContextCompressor::compress` before
    /// it mutated the conversation. `local_agent_service` reads this
    /// in the same `persist_session_with_summary` transaction that
    /// writes `sessions.compressed_summary`, and forwards it to
    /// `SessionPersistence::archive_compressed_turns` so the
    /// original wording is durable in `compression_archives`.
    latest_compression_archived_turns: std::sync::Mutex<Option<Vec<Message>>>,
    /// (B): metrics from the most recent compression
    /// (original/compressed turns + tokens + savings). Forwarded
    /// alongside the archived turns so `save_session_atomic` can
    /// write `compression_summary_history` in the same transaction.
    latest_compression_metrics: std::sync::Mutex<Option<crate::compression::CompressionMetrics>>,
    /// Most recent archive refs produced by
    /// [`archive_context_section`]. Surfaced to the frontend so the
    /// user can open the archive file at a specific node anchor.
    /// See `crates/hermix-agent/src/archive.rs` (v2: ).
    last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>>,
    /// P1 : background-spawned compression bookkeeping.
    /// When `Some`, the agent loop has fired a `tokio::spawn` to
    /// compress a snapshot of `ctx.messages` and is waiting for it
    /// to finish — the agent loop is *not* blocking on the LLM
    /// summary call. On every `do_pre_turn_setup` we poll this
    /// handle; when the spawn is ready, the result is rolled into
    /// `ctx.messages` and the slot cleared. When we re-trigger the
    /// spawn path and a previous spawn is still in flight, we drop
    /// the old slot (the spawned task keeps running in the
    /// background but its outcome is discarded — see
    /// `CompressionInFlight` design notes in `types.rs`).
    /// `std::sync::Mutex` because the slot is only ever touched from
    /// the single-threaded agent loop body — no contention, so we
    /// avoid the `Send`/`Sync` tax of `tokio::sync::Mutex` here.
    compression_in_flight: std::sync::Mutex<Option<types::CompressionInFlight>>,
    /// (memory recall gating): `false` until the first full
    /// memory recall completes. Once `true`, subsequent `memory_prefetch`
    /// calls use a smaller limit to avoid re-injecting the same memory
    /// every turn — the LLM already has it in the active context window.
    memory_recall_seeded: std::sync::atomic::AtomicBool,
    /// (prompt caching): three-tier prompt cache. Holds the
    /// rendered stable layer (identity, directions, tool block,
    /// personality, workspace, provider hint) and context layer
    /// (memory / skills / context files), each independently keyed so
    /// a context-store mutation does not force the larger stable layer
    /// to re-render. The volatile layer (timestamp + session_id) is
    /// rebuilt every turn and not cached.
    prompt_cache: std::sync::Mutex<crate::agent_helpers::PromptCache>,
    /// Monotonic epoch bumped when memory/skills/context-files stores
    /// mutate; forms the key for the context cache layer. The read
    /// side is wired through `prompt_assembly::build_context_layer`.
    /// Bump this via `context_epoch.fetch_add(1, Ordering::Relaxed)`
    /// when memory/skills/context-files are modified.
    context_epoch: std::sync::atomic::AtomicU64,
    /// Epoch at which `config.stored_system_prompt` was last (re)built.
    /// `resolve_initial_system_prompt` compares this against `context_epoch`
    /// to detect that a memory/skill/context-file store mutated since the
    /// stored prompt was frozen, forcing a rebuild instead of reusing the
    /// stale frozen string. (makes `memory` tool edits take
    /// effect in-session instead of only after a session restart.)
    stored_prompt_epoch: std::sync::atomic::AtomicU64,
    /// Optional embedding provider for vector recall in memory prefetch.
    /// When set, `memory_prefetch` generates embeddings and passes them
    /// to `MemoryQuery` for cosine similarity search.
    embedding_provider: Option<Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>>,
    /// Background task JoinHandles for graceful shutdown. When the
    /// AgentLoop is dropped, these tasks are aborted to prevent
    /// inconsistent state from mid-flight writes.
    background_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Drop for AgentLoop {
    fn drop(&mut self) {
        // Abort all background tasks to prevent inconsistent state
        // from mid-flight writes (e.g. skill/memory review writing
        // to disk while the process is shutting down).
        if let Ok(tasks) = self.background_tasks.lock() {
            for handle in tasks.iter() {
                handle.abort();
            }
        }
    }
}

/// Encapsulates the 3 tracking fields the context-pressure warning
/// logic uses. Lifted out of [`LoopState`] so the cooldown/throttle
/// rules live in one place (and can be unit-tested without spinning
/// up an `AgentLoop`).
/// replaces 3 fields (`context_pressure_warned_at`,
/// `context_pressure_last_warn_at`, `context_pressure_last_warn_percent`)
/// scattered across `LoopState`. The shared [`should_emit_warning`]
/// method now owns the cooldown + tier-upgrade + percent-step rules.
/// Physically split to `context_pressure.rs` ; re-exported
/// here so existing `state.context_pressure.*` call paths compile unchanged.
pub use crate::context_pressure::ContextPressureMonitor;

/// cost-guard bookkeeping consolidated into one
/// struct. The four sibling fields (`accumulated_usage`,
/// `session_cost_usd`, `cost_warned`, `forced_runtime_route`) used to
/// live directly on `LoopState` and were passed one-by-one as 4 sibling
/// `&mut` arguments into [`AgentLoop::process_post_response`]. Folding
/// them into one struct:
/// - shrinks `LoopState` from 14 to 10 fields,
/// - shrinks `process_post_response` from 7 to 3 mutable args (3 scalars
/// stay as primitives; the bookkeeping is now `&mut CostGuard`),
/// - keeps the 4-field atomic-mutation at the cost-guard code path
/// where it belongs (a single helper can read + write the state),
/// - lets `publish_from` drop another 4 args.
/// The 4 fields stay co-located on `self.cost_guard` and mutate in
/// place via the `record_usage` / `apply_route_downgrade` methods.
/// Reset / `Default` produce the same all-zeros state as before, so
/// `LoopState::default()` semantics are unchanged.
/// Physically split to `cost_guard.rs` ; re-exported here
/// so existing `state.cost_guard.*` call paths compile unchanged.
/// `TurnRuntimeRoute` was lifted to `pub(crate)` so the new module
/// can name it; the field visibility stays `pub(crate)` so the
/// runtime publish path (`state.cost_guard.accumulated_usage`) keeps
/// working without accessor-method churn.
pub use crate::cost_guard::{CostGuard, CostGuardAction};
pub(crate) use crate::cost_routing::RouteState;

/// per-loop retry counters consolidated into one
/// struct so `LoopState` doesn't have to carry 3 sibling `u32` fields and
/// `publish_from` doesn't have to take 3 sibling `u32` arguments.
/// All three counters follow the same shape:
/// - `bump_*` saturating-increments and returns the new value.
/// - `reset_*` clears the counter (called on the success path).
/// - The `< max` / `>= max` comparison stays at the call site because
/// the action on max differs between the three (finalize vs.
/// continue-with-error vs. fall-through) — folding that decision
/// into the helper would force one path on all three.
/// Physically split to `retry_counters.rs` ; re-exported
/// here so existing `state.retry_counters.*` call paths compile
/// unchanged.
pub use crate::retry_counters::RetryCounters;

/// (follow-up): per-run LLM-result caches consolidated
/// into one struct. The two sibling `Option` fields
/// (`last_checkpoint_messages`, `last_content_with_tools`) used to live
/// directly on `LoopState` and were bridged back from `run()`'s
/// loop-locals via 2 sibling arguments in `publish_from`. Folding them
/// into one struct:
/// - shrinks `LoopState` by 2 fields (8 → 6),
/// - shrinks `publish_from` from 3 args to 1 arg (only `total_turns`
/// remains bridged),
/// - lets `run()` mutate the caches in place via
/// `store_checkpoint` / `store_content_with_tools` /
/// `take_content_with_tools` instead of accumulating to a local
/// alias and re-publishing each iteration,
/// - collapses 2 helper-arg slots (`&mut Option<String>` for
/// `last_content_with_tools`) to 1 (`&mut MessageCache`).
/// The 2 fields stay co-located on `self.message_cache` and mutate
/// in place. Reset / `Default` produce the same all-`None` state as
/// before, so `LoopState::default()` semantics are unchanged.
/// Physically split to `message_cache.rs` ; re-exported
/// here so existing `state.message_cache.*` call paths compile
/// unchanged.
pub use crate::message_cache::MessageCache;

/// (follow-up): turn-level timing bookkeeping consolidated
/// into one struct. The two sibling `u64` fields (`total_api_time_ms`,
/// `total_tool_time_ms`) used to live directly on `LoopState` and were
/// bridged back from `run()`'s loop-locals via 2 sibling arguments in
/// `publish_from`. Folding them into one struct:
/// - shrinks `LoopState` by 2 fields (10 → 8),
/// - shrinks `publish_from` from 5 args to 3 args,
/// - lets `run()` mutate the timings in place via `record_api` /
/// `record_tool` instead of accumulating to a local `_total_*` and
/// re-publishing each iteration (the local accumulation was the
/// pre-extraction pattern that the bridge set up for
/// `total_turns`; timings don't need that bridge because nothing
/// reads `state.timings` from outside this file — they're just
/// collected for future inspection / metrics).
/// The 2 fields stay co-located on `self.timings` and mutate in
/// place. Reset / `Default` produce the same all-zeros state as
/// before, so `LoopState::default()` semantics are unchanged.
/// Physically split to `turn_timings.rs` ; re-exported
/// here so existing `state.timings.*` call paths compile unchanged.
pub use crate::turn_timings::TurnTimings;

// ---------------------------------------------------------------------------
//Backwards-compat re-exports (P2.2.4, )
// ---------------------------------------------------------------------------


pub mod config;
pub mod memory;
pub mod runtime;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod stub_provider;
#[cfg(test)]
mod scripted_provider;
#[cfg(test)]
mod replay_tests;

#[derive(Debug, Clone)]
pub(crate) struct FileChangeInfo {
    pub path: String,
    pub change_type: &'static str, // "created" | "modified" | "deleted" | "renamed"
    pub summary: Option<String>,
}

/// the per-attempt classification that sits
/// inside the `let response = loop { ... }` retry block in both
/// `run()` and `run_stream()`. Identical ~40 lines in both call
/// sites — has_tools / visible_text / reasoning-retry / empty-retry /
/// exhausted. Extracted here so the two retry loops can keep their
/// own transport (sync vs stream-first) and call sites can `match`
/// on the resulting action.
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
    /// No longer constructed — reasoning-only responses
    /// now Break immediately. Kept for pattern-match exhaustiveness
    /// and future use.
    #[allow(dead_code)]
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

//Physically moved to `cost_guard.rs` alongside the
// rest of `cost_guard::*`. Kept behind `#[cfg(test)]` because the only
// callers (test_merge_usage / test_merge_usage_none) live in the
// `mod tests` block below — without this gate Rust lints the import
// as unused in the lib build while the tests still need it.
#[cfg(test)]
use crate::cost_guard::merge_usage;
