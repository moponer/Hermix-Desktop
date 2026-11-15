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
    /// provider 上报的「前缀缓存命中」输入 token
    /// （OpenAI `prompt_tokens_details.cached_tokens` / DeepSeek
    /// `prompt_cache_hit_tokens` / Anthropic `cache_read_input_tokens`）。
    ///
    /// 2026-10-30 加：此前 turn report 只有 prompt/completion，日志里看不出
    /// 「这一次命中了多少」—— 排查 86% 命中率只能去翻 `sessions.db`。
    /// 命中数必须和 prompt 数出现在同一行，才能就地把冷启动认出来。
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// provider 上报的缓存写入 token（Anthropic `cache_creation_input_tokens`）。
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// 这一轮**实际**用的模型（provider 回显的 `LlmResponse.model`）。
    ///
    /// 2026-10-30 加：会话钉住的模型与 `config.yaml` 的 `model:` 可能不一致
    /// （实测出现过 `minimax:minimax-m3.1-flash` 被 provider 判为未知模型，
    /// 而配置文件里写的是 `MiniMax-M3.1-Flash`）。没有这一列，"这一轮到底用
    /// 了哪个模型"只能靠猜，无法判断是会话覆盖、路由还是 provider 归一化。
    #[serde(default)]
    pub model: String,
    /// Total tokens in context after this turn.
    pub context_tokens: u64,
    /// 该模型解析出的上下文窗口（0 = 未解析到）。
    ///
    /// 2026-10-30 加：只记 `context_tokens` 时，日志里 22.9 万这个数字无法判断
    /// "占窗口几成"，也就无法解释"为什么压缩没触发 / 为什么每轮要 140 秒"。
    /// 两者一起记，比值一眼可见。
    #[serde(default)]
    pub context_max_tokens: u64,
    /// 压缩阈值 token（`(window - output_reserve) * pressure_ratio`）。
    ///
    /// 2026-10-30 加：长窗口模型（实测 1,048,576）把阈值推到 ~94 万，
    /// 22 万 token 的会话只占 21%，压缩一辈子不触发。只记 `context_tokens`
    /// 时这个"永远不触发"看不出来；把阈值同排打出来，"离压缩还有多远"
    /// 才是可验证的事实。
    #[serde(default)]
    pub context_threshold_tokens: u64,
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
            model = %self.model,
            prompt_tokens = self.prompt_tokens,
            completion_tokens = self.completion_tokens,
            cache_read_tokens = self.cache_read_tokens,
            // 命中率直接给到行里：`cache_read / prompt`。冷启动就是 0.0，
            // 会话内的正常值应 >= 95。
            cache_hit_pct = if self.prompt_tokens > 0 {
                (self.cache_read_tokens as f64 / self.prompt_tokens as f64 * 1000.0).round() / 10.0
            } else {
                0.0
            },
            context_tokens = self.context_tokens,
            context_max = self.context_max_tokens,
            context_threshold = self.context_threshold_tokens,
            // 压缩阈值是百分比判据 —— 日志里直接给比例，免得读者自己算。
            context_pct = if self.context_max_tokens > 0 {
                (self.context_tokens as f64 / self.context_max_tokens as f64 * 100.0).round() as u64
            } else {
                0
            },
            outcome = ?self.outcome,
            mode = ?self.mode,
            task_id = ?self.task_id,
            "turn report"
        );
    }
}
