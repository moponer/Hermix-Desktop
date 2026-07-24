// ---------------------------------------------------------------------------
// Data structures for the AgentLoop (extracted from agent_loop.rs
// 2026-07-06, God Object refactor Phase 1)
// ---------------------------------------------------------------------------

use std::sync::Arc;

use hermix_core::{Message, ToolSchema};

use crate::context::ContextManager;
use crate::context_pressure::ContextPressureMonitor;
use crate::cost_guard::CostGuard;
use crate::cost_routing::RouteState;
use crate::credential_pool::CredentialPool;
use crate::message_cache::MessageCache;
use crate::retry_counters::RetryCounters;
use crate::smart_model_routing::{ApiMode, TurnRouteSignature};
use crate::turn_timings::TurnTimings;

// ---------------------------------------------------------------------------
// Runtime route (model + provider selection per turn)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Prepared run context (output of prepare_run_context)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// All mutable loop-local state
// ---------------------------------------------------------------------------

/// All mutable loop-local state shared between `run()` and `run_stream()`.
/// Centralized to avoid declaring 17+ individual locals in both methods
/// (reduces duplication and eliminates the "forget to sync" bug).
/// /// Phase 1 (2026-07-03) introduces the struct. Phase 1.1 wires the
/// 14 run() + 17 run_stream() locals through it via `publish_from`.
/// Phase 5 (+ 2026-07-04) folds 9 inline fields into 4
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
    /// (AL-8): convergence detector for no-progress / error
    /// loop / similar content detection. Goes beyond simple tool dedup
    /// to catch agents that are "working" (calling different tools) but
    /// not making real progress.
    pub(crate) convergence_detector: crate::convergence_detector::ConvergenceDetector,
    /// Phase 19 (A): keys (canonical_key) most recently surfaced
    /// by `recall_scheduler`. The scheduler reads this list to
    /// detect drift against the current user turn (cheap first-
    /// token compare; no embedding). On a recall hit we replace
    /// it with the new top-k.
    pub(crate) last_recall_keys: Vec<String>,
}

// ---------------------------------------------------------------------------
// Background-spawned compression bookkeeping (P1, 2026-07-18)
// ---------------------------------------------------------------------------

/// Outcome of one background-spawned compression attempt.
/// /// Produced by the `tokio::spawn`-ed compressor task and consumed by
/// `apply_compression_result` when the agent loop's next
/// `do_pre_turn_setup` polls the in-flight handle. The struct holds
/// everything we need to roll the compressed state forward without
/// having to take `compressor.lock().await` again on the apply side.
/// /// `compressed_base` is the snapshot the compressor produced (typically
/// `[kept_head, summary_msg]`). `new_messages_since_snapshot` is left
/// empty here — the agent loop splices them in at apply time so the
/// compressor never has to know what the live `ctx` looks like after
/// it started.
pub(crate) struct CompressionOutcome {
    /// Compressed version of the *snapshot* slice (i.e. everything that
    /// existed in `ctx.messages` at trigger time). When applied we
    /// splice in messages added after the trigger point.
    pub compressed_base: Vec<Message>,
    /// Archive refs the spawn task produced when calling
    /// `archive_context_section` on the snapshot. The apply path
    /// forwards these to `self.last_archive_refs` so the frontend
    /// "see archived context" affordance lights up. Empty when the
    /// archive pass found nothing to unload.
    pub archive_refs: Vec<crate::archive::ArchiveRef>,
    /// True when the spawn task completed without panicking and the
    /// LLM summary call (if any) returned successfully. Currently
    /// only logged; the apply path re-locks the compressor to read
    /// the persisted summary / metrics directly so we don't have to
    /// mirror them through this struct.
    pub success: bool,
}

/// In-flight bookkeeping for one background compression.
/// /// Lives on the `AgentLoop` (`compression_in_flight: Mutex<Option<Self>>`)
/// so the agent loop's main path can poll it on every turn setup and
/// roll the result in without re-locking the compressor. We hold the
/// `JoinHandle` rather than a `tokio::sync::oneshot::Receiver` because
/// the spawn task itself owns the result; `JoinHandle` lets us await
/// it with a timeout.
pub(crate) struct CompressionInFlight {
    /// The spawn task. Awaitable with a timeout; on success returns
    /// the `CompressionOutcome`.
    pub handle: tokio::task::JoinHandle<CompressionOutcome>,
    /// `ctx.messages.len()` at trigger time — the apply path uses this
    /// to know which messages in the live `ctx` were *added after* the
    /// spawn fired and must be preserved verbatim.
    pub snapshot_len: usize,
    /// Total tokens at trigger time, used by the apply path to
    /// recompute `total_tokens_saved` after the merge.
    pub snapshot_tokens: usize,
    /// Wall-clock instant when the spawn fired; the apply path can
    /// use it to log "compression finished after X ms" without
    /// tracking latency separately.
    pub started_at: std::time::Instant,
    /// Snapshot of `ctx.max_context_tokens` at trigger time so the
    /// apply path can recompute pressure ratios without re-reading
    /// the context (which would require holding `ctx` by `&`).
    pub max_context_tokens: usize,
}

// ---------------------------------------------------------------------------
// Explicit state-machine phases (AL-6, 2026-07-09)
// ---------------------------------------------------------------------------

/// Explicit representation of the Agent Loop's state machine.
/// Each variant maps to a distinct phase in the perception-decision-action
/// cycle. Previously these phases were implicit — buried in `if`/`else`,
/// `match`, `continue`, and `return` statements scattered across ~1,700
/// lines of `run()` + `run_stream()`. Making them explicit serves three
/// purposes:
/// /// 1. **Documentation** — a new reader can see all possible states at a glance.
/// 2. **Observability** — emitted via `tracing` so dashboards can show
/// which phase each turn is in.
/// 3. **Future unification** — when `run()` and `run_stream()` merge into
/// `run_engine`, this enum becomes the match target of the single loop.
/// /// The phases flow in order, but some can skip (e.g. `PostLlmCall` →
/// `Finalize` if no tool calls) or loop back (`PostToolExec` → `PreTurnSetup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LoopPhase {
    /// Initial setup: sanitize messages, route tools, build system prompt,
    /// hydrate todo store, prefetch memory. Executed once before the loop.
    Init,
    /// Start of each turn: interrupt check, max-turns guard, turn counter,
    /// OAuth refresh, checkpoint, memory sync, hook invocation.
    PreTurnSetup,
    /// Pre-LLM prefetch: intent classification, safe tool dispatch,
    /// thinking-mode selection.
    Prefetch,
    /// LLM API call with transport retry + semantic empty/thinking recovery.
    /// Contains the inner retry loop (`classify_post_llm_response`).
    LlmCall,
    /// Post-LLM response processing: cost guard, finalize/continue decision,
    /// assistant message persistence.
    PostLlmCall,
    /// Tool call validation: invalid name check, invalid JSON check,
    /// argument normalization, cross-turn dedup split.
    ToolValidation,
    /// Tool execution: dispatch to tool backends, collect results.
    ToolExecution,
    /// Post-tool iteration: hooks, file-change events, budget enforcement,
    /// context pressure, auto-compression.
    PostToolExec,
    /// Session finalized: persist session, emit terminal events,
    /// return `AgentResult`.
    Finalize,
}

impl LoopPhase {
    /// Short label for tracing spans.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::PreTurnSetup => "pre_turn_setup",
            Self::Prefetch => "prefetch",
            Self::LlmCall => "llm_call",
            Self::PostLlmCall => "post_llm_call",
            Self::ToolValidation => "tool_validation",
            Self::ToolExecution => "tool_execution",
            Self::PostToolExec => "post_tool_exec",
            Self::Finalize => "finalize",
        }
    }

    /// Map this fine-grained phase to a coarse-grained orchestration
    /// stage (P3-1). The four stages correspond to the
    /// perception-reasoning-action-observation cycle:
    /// /// - **Perception** — Init, PreTurnSetup, Prefetch
    /// - **Reasoning** — LlmCall, PostLlmCall
    /// - **Action** — ToolValidation, ToolExecution
    /// - **Observation** — PostToolExec, Finalize
    /// /// Used by `TurnReport` for observability and by the frontend
    /// for progress display.
    pub(crate) fn as_orchestration_stage(&self) -> crate::orchestrator::OrchestrationStage {
        use crate::orchestrator::OrchestrationStage;
        match self {
            Self::Init | Self::PreTurnSetup | Self::Prefetch => {
                OrchestrationStage::Perception
            }
            Self::LlmCall | Self::PostLlmCall => OrchestrationStage::Reasoning,
            Self::ToolValidation | Self::ToolExecution => OrchestrationStage::Action,
            Self::PostToolExec | Self::Finalize => OrchestrationStage::Observation,
        }
    }
}

impl std::fmt::Display for LoopPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Post-response action
// ---------------------------------------------------------------------------

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
