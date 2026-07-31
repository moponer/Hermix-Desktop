//! Structured Turn Report for observability (AL-10, ).
//!
//! Each completed turn in the agent loop produces a `TurnReport` that
//! captures what happened — which phase was entered, how long it took,
//! what tools were called, what errors occurred, and what the outcome
//! was. This is emitted via `tracing::info!` so dashboards and log
//! aggregators can reconstruct the full turn timeline.
//!
//! The report is intentionally lightweight (all owned data, no
//! references) so it can be serialized and shipped to the dashboard
//! without holding any locks on the agent loop state.
//!
//! : extended with `mode`, `task_id`, and `stage`
//! fields to support the orchestration engine's Task-level tracking.

use serde::Serialize;

use crate::agent_loop::LoopPhase;
use crate::convergence_detector::ConvergenceReason;
use crate::orchestrator::OrchestrationStage;

/// A single tool call within a turn report.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnToolCall {
    pub name: String,
    pub is_error: bool,
    pub is_replay: bool,
    pub duration_ms: u64,
}

/// Outcome of a turn.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // P1-1: only Continue is emitted today; other variants for future finalize/abort reporting
pub(crate) enum TurnOutcome {
    /// Turn completed normally, proceeding to next turn.
    Continue,
    /// Turn concluded the run (finalized).
    Finalize,
    /// Turn was interrupted by graceful interrupt.
    Interrupted,
    /// Turn hit a convergence signal.
    Converged { reason: ConvergenceReason },
    /// Turn hit max turns.
    MaxTurns,
    /// Turn hit cost guard.
    CostGuardTripped,
    /// Turn hit fatal validation error.
    FatalValidation,
}

/// Structured report for a single agent loop turn.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnReport {
    /// Turn number (1-based).
    pub turn: u32,
    /// Which phase the turn ended in.
    pub end_phase: LoopPhase,
    /// Coarse-grained orchestration stage . Mapped from
    /// `end_phase` via `LoopPhase::as_orchestration_stage()`.
    pub stage: OrchestrationStage,
    /// API call duration in ms.
    pub api_ms: u64,
    /// Tool execution duration in ms.
    pub tool_ms: u64,
    /// Tool calls made this turn.
    pub tool_calls: Vec<TurnToolCall>,
    /// Number of replays (dedup hits).
    pub replay_count: usize,
    /// Whether the LLM produced visible text.
    pub has_visible_text: bool,
    /// Whether the LLM produced thinking content.
    pub has_thinking: bool,
    /// Token usage for this turn (prompt + completion).
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Total tokens in context after this turn.
    pub context_tokens: u64,
    /// Outcome of this turn.
    pub outcome: TurnOutcome,
    /// Working mode for the Task associated with this run .
    /// `None` when running outside a Task context (e.g. legacy sessions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<hermix_core::TaskMode>,
    /// Task ID associated with this run .
    /// `None` when running outside a Task context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl TurnReport {
    /// Emit the report via `tracing::info!`.
    pub fn emit(&self) {
        tracing::info!(
            turn = self.turn,
            end_phase = %self.end_phase,
            stage = %self.stage,
            api_ms = self.api_ms,
            tool_ms = self.tool_ms,
            tool_count = self.tool_calls.len(),
            replay_count = self.replay_count,
            has_visible_text = self.has_visible_text,
            has_thinking = self.has_thinking,
            prompt_tokens = self.prompt_tokens,
            completion_tokens = self.completion_tokens,
            context_tokens = self.context_tokens,
            outcome = ?self.outcome,
            mode = ?self.mode,
            task_id = ?self.task_id,
            "turn report"
        );
    }
}
