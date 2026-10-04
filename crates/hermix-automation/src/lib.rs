//! # hermix-automation
//!
//! Automation engine with three-table separation :
//!
//! - **`automations`** — definition table: name, prompt, schedule, status
//! - **`automation_runtime_state`** — runtime table: last_run, next_run, run_count
//! - **`automation_runs`** — history table: each execution record
//!
//! This separation follows the WorkBuddy architecture: scheduling logic is
//! decoupled from execution history, enabling independent queries and
//! efficient updates without touching the definition row.
//!
//!

pub mod repository;
pub mod scheduler;
pub mod service;

pub use repository::{AutomationRepository, AutomationRepositoryError};
pub use scheduler::{AutomationScheduler, TriggerFn};
pub use service::{
    backoff_delay_ms, AutomationService, AutomationServiceError, MAX_DISPATCH_ATTEMPTS,
};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Schedule type for an automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleType {
    /// Recurring schedule using a cron expression.
    Recurring,
    /// One-time schedule at a specific timestamp.
    Once,
}

/// What caused a run to be claimed.
///
/// Part of the runId contract, so it is a closed set rather than a free
/// string: `scheduled` is the scheduler tick, `manual` is a user
/// clicking "run now". A manual run deliberately does **not** carry a
/// `scheduled_at` — the user asked for one run, not for a specific slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTrigger {
    Scheduled,
    Manual,
}

impl RunTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunTrigger::Scheduled => "scheduled",
            RunTrigger::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scheduled" => RunTrigger::Scheduled,
            "manual" => RunTrigger::Manual,
            _ => return None,
        })
    }
}

/// Compose the runId for a `(automation, trigger, scheduled_at)` triple.
///
/// The contract's purpose: two ticks that resolve to the same scheduled
/// slot are **the same run**, not two. A clock jump, a restart, or two
/// ticks inside one interval all land on the same `scheduled_at` and
/// therefore the same id, so the second one is rejected as a duplicate
/// instead of double-firing.
///
/// `scheduled_at` is the slot that was *due*, not the wall clock at
/// claim time — otherwise a 30s tick landing 1ms after midnight would
/// mint a fresh id and the run would be impossible to deduplicate.
///
/// **Manual triggers must always pass a wall-clock timestamp.** Without one
/// the id degenerates to `{automation}:manual`, which is constant — so the
/// second "run now" click of the same day is deduped against the first
/// and silently does nothing. There is no slot to collapse for a manual
/// run: every click is a distinct user intent, and only the single-flight
/// check (not this key) is meant to stop two of them racing.
pub fn compose_run_id(automation_id: &str, trigger: RunTrigger, scheduled_at: Option<i64>) -> String {
    match scheduled_at {
        Some(ts) => format!("{}:{}:{}", automation_id, trigger.as_str(), ts),
        None => format!("{}:{}:{}", automation_id, trigger.as_str(), now_millis()),
    }
}

/// Wall clock in milliseconds. Split out so tests can see the dedup key's
/// dependency on it without mocking the clock.
pub(crate) fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Whether a dispatch failure is worth retrying.
///
/// The scheme note said to reuse `classifyStreamErrorKind`, but that
/// function lives in the desktop front-end (`app/streamEvents.ts`) and
/// only reads strings — `hermix-automation` is a plain Rust crate with no
/// access to it. `AgentError` is already a tagged enum, so matching on
/// variants *is* the classifier; re-deriving the same judgement from
/// error text here would be the string-matching the note wanted to avoid.
///
/// The line is drawn at "could the same call succeed later without the
/// user changing anything":
///
/// - transient — the outside world was busy or briefly broken (rate
///   limit, provider overload, gateway blip, network, timeout). Retrying
///   is safe because **the agent never started**, so there are no side
///   effects to repeat.
/// - permanent — a local precondition is wrong (bad config, missing
///   credentials, an invalid tool call). Retrying burns attempts and
///   delays the real error reaching the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Transient,
    Permanent,
}

/// Classify a failure that came back from the dispatch call itself.
///
/// `max_runs`-style caps are irrelevant here; only the error's nature
/// matters. Note this is orthogonal to `ScheduleType` — a `Once` job hits
/// transient provider outages just as easily as a `Recurring` one, and a
/// `Recurring` job can be permanently misconfigured.
///
/// ## Scope: this only sees pre-agent configuration failures
///
/// In practice this is reached with `AgentError::Config` and little else.
/// `AgentLoop::run()` has no `return Err` at all — an LLM or transport
/// failure is converted to `Ok(..)` with `finish_reason = "error"` so the
/// transcript still gets persisted (`hermix-agent/src/run_loop.rs`, the
/// branch commented "treating as graceful interrupt for persistence").
/// So `LlmApi` / `Gateway` / `RateLimited` / `Timeout` never surface here
/// for an automation: by the time one of those would be raised, the agent
/// has already started, and the caller must classify it by *how far the
/// loop got* rather than by error variant — see
/// [`crate::dispatch_outcome`].
///
/// The transient arms are kept because `send_message` does return `Err`
/// from pre-flight setup today, and a future refactor may widen that.
/// What they must never be used for is deciding to replay a run that
/// already executed tools.
pub fn classify_dispatch_failure(err: &hermix_core::AgentError) -> FailureClass {
    use hermix_core::AgentError;
    match err {
        // World-state problems: the same request can work later.
        AgentError::LlmApi(_) | AgentError::Gateway(_) | AgentError::Timeout(_) => {
            FailureClass::Transient
        }
        AgentError::RateLimited { .. } => FailureClass::Transient,
        AgentError::Io(_) => FailureClass::Transient,
        // Local precondition problems: nothing changes without the user.
        AgentError::Config(_) | AgentError::AuthFailed(_) | AgentError::InvalidToolCall(_) => {
            FailureClass::Permanent
        }
        // `ToolExecution` / `ContextTooLong` / `MaxTurnsExceeded` are
        // agent-execution outcomes, not dispatch outcomes — a run that
        // reached the agent must not be retried on this signal at all.
        AgentError::ToolExecution(_)
        | AgentError::ContextTooLong
        | AgentError::MaxTurnsExceeded
        | AgentError::Interrupted { .. } => FailureClass::Permanent,
    }
}

/// What an automation run should be recorded as, and whether a retry is safe.
///
/// The whole point of this enum is that the caller cannot read safety off a
/// single error signal. `AgentLoop::run()` reports both "the first LLM call
/// never landed" and "the agent ran nine tool calls then died" as `Ok`
/// with `finish_reason = "error"`; the only thing separating them is
/// `total_turns`, which the loop bumps *before* each LLM call. So a retry
/// decision needs the turn count and the error together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The agent produced a natural completion.
    Succeeded,
    /// The run failed and **replaying it could repeat side effects** —
    /// the agent got at least one response back, so tools may have run.
    /// Never retried, even when the underlying cause looks transient.
    Failed { retryable: bool },
    /// The failure provably happened before any tool ran (`total_turns`
    /// is `1` and `last_error` names a world-state problem), so a replay
    /// starts from a clean slate.
    DispatchFailed { retryable: bool },
}

impl DispatchOutcome {
    /// Whether a retry has been scheduled for this outcome.
    pub fn is_retry_scheduled(&self) -> bool {
        matches!(
            self,
            Self::Failed { retryable: true } | Self::DispatchFailed { retryable: true }
        )
    }
}

/// Derive the run outcome from an agent reply.
///
/// `reply` is the result of `AgentService::send_message`; the caller passes
/// the error text separately because a failed *transport* has no reply at
/// all, and mixing the two cases here would blur the distinction this
/// function exists to make.
///
/// `total_turns == 1` means the first LLM call was attempted and nothing
/// came back — no tool could have run. `>= 2` means at least one response
/// arrived, which is the point where replay stops being safe.
pub fn dispatch_outcome(
    reply: &hermix_core::traits::AgentReply,
) -> DispatchOutcome {
    let errored = reply.finish_reason.as_deref() == Some("error");
    if !errored {
        return DispatchOutcome::Succeeded;
    }
    if reply.total_turns <= 1 {
        // First call, no response, therefore no tool. Whether it is worth
        // retrying depends on the error text, which is the only handle we
        // have — `last_error` is a raw string, not a typed error.
        let transient = is_transient_error_text(reply.last_error.as_deref().unwrap_or_default());
        DispatchOutcome::DispatchFailed { retryable: transient }
    } else {
        DispatchOutcome::Failed { retryable: false }
    }
}

/// Heuristic over a raw error string.
///
/// The loop only preserves `last_error` as text (`run_loop.rs` converts the
/// typed `AgentError` with `e.to_string()`), so classification has to run
/// over the message. Deliberately narrow: it errs toward "permanent",
/// because the cost of a wrong `true` is replaying a run whose tools
/// already fired, while the cost of a wrong `false` is one missed retry.
fn is_transient_error_text(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "429", "rate limit", "too many requests", "503", "502", "504",
        "service unavailable", "bad gateway", "gateway timeout",
        "timeout", "timed out", "connection reset", "connection refused",
        "connection closed", "temporarily unavailable", "eof",
    ];
    TRANSIENT.iter().any(|k| t.contains(k))
}

impl std::fmt::Display for ScheduleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recurring => f.write_str("recurring"),
            Self::Once => f.write_str("once"),
        }
    }
}

/// Automation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AutomationStatus {
    Active,
    Paused,
}

impl std::fmt::Display for AutomationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => f.write_str("ACTIVE"),
            Self::Paused => f.write_str("PAUSED"),
        }
    }
}

/// Automation definition — maps to the `automations` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Automation {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The prompt to send to the agent when triggered.
    pub prompt: String,
    /// Schedule type: recurring or once.
    pub schedule_type: ScheduleType,
    /// RRULE/cron string for recurring schedules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// ISO8601 timestamp for one-time schedules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    /// Active or Paused.
    pub status: AutomationStatus,
    /// Valid from (ISO8601). If set, automation won't fire before this date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    /// Valid until (ISO8601). If set, automation won't fire after this date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Working directory paths (JSON array). If set, constrains file tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwds: Option<String>,
    /// Connector IDs (JSON array). Which MCP connectors to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_ids_json: Option<String>,
    /// Expert ID for the agent persona.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expert_id: Option<String>,
    /// Model override (e.g. "openai:gpt-4o").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Conversation this automation was created in. `Some(session_id)`
    /// when it came from a `cronjob` tool call inside a chat (the
    /// registry stamps `_session_id` into tool params — see
    /// `ToolRegistry::inject_session_id`); `None` when created from
    /// the settings UI. The scheduler uses it to route results back
    /// into that conversation instead of a private `automation:{id}`
    /// session. **Not consumed yet** — the routing branch lands with
    /// the schedule-model upgrade; this column is the data half.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_session_id: Option<String>,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: i64,
    /// Last update timestamp (Unix milliseconds).
    pub updated_at: i64,
}

/// Runtime state — maps to the `automation_runtime_state` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRuntimeState {
    /// FK to `automations.id`.
    pub automation_id: String,
    /// Last execution timestamp (Unix milliseconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    /// Next scheduled execution (Unix milliseconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// Total number of executions.
    pub run_count: i64,
}

/// Lifecycle state of a single automation run.
///
/// The transition graph:
///
/// ```text
///                    ┌──────────► skipped          (claimed but declined to execute)
///                    │
///   (claim) ──► claimed ──► dispatched ──┬──► succeeded
///                    │                   ├──► failed
///                    │                   └──► failed_to_dispatch   (dispatch failed, retries exhausted)
///                    │
///                    └──► abandoned                             (process died while in flight)
/// ```
///
/// Two distinctions carry real meaning and must not be collapsed:
///
/// - **`skipped` is not a failure.** It means "claimed successfully, then
///   chose not to execute" — another run was still in flight, or
///   `valid_until` had already passed. The UI must not paint it red.
/// - **`failed` is not `failed_to_dispatch`.** The first is the agent
///   having run and failed (side effects may already have happened — do
///   not retry); the second is the dispatch layer failing before the
///   agent ever started (safe to retry with backoff).
///
/// No `CHECK` constraint is added on the DB column: `ALTER TABLE` cannot
/// add one to an existing table (it would need a rebuild), the type
/// system already rejects misspellings at compile time, and the payoff
/// would only be blocking typos. See the schedule-model-upgrade note,
/// "实施要点 2".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Claimed, not yet handed to the agent. Written **synchronously**
    /// before the async spawn so a slow spawn cannot let the next tick
    /// fire a duplicate.
    Claimed,
    /// Handed to the agent, in flight.
    Dispatched,
    /// Agent ran to completion successfully.
    Succeeded,
    /// Agent ran and failed. Side effects may have occurred — never retried.
    Failed,
    /// Dispatch layer failed (agent never started) and retries are exhausted.
    FailedToDispatch,
    /// Claimed, then declined to execute. **Not a failure.** The engine
    /// writes these for "another run is in flight" and "schedule has
    /// ended" (`valid_until` in the past).
    Skipped,
    /// Process restarted while the run was in flight; reaped at startup.
    Abandoned,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Claimed => "claimed",
            RunStatus::Dispatched => "dispatched",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::FailedToDispatch => "failed_to_dispatch",
            RunStatus::Skipped => "skipped",
            RunStatus::Abandoned => "abandoned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "claimed" => RunStatus::Claimed,
            "dispatched" => RunStatus::Dispatched,
            "succeeded" => RunStatus::Succeeded,
            "failed" => RunStatus::Failed,
            "failed_to_dispatch" => RunStatus::FailedToDispatch,
            "skipped" => RunStatus::Skipped,
            "abandoned" => RunStatus::Abandoned,
            // Pre-state-machine rows used a `running` / `success` pair.
            // Map them onto the nearest state instead of failing the read:
            // losing a run's history because of a rename is worse than a
            // lossy label. `running` was really "in flight", and at the
            // time of the upgrade a stale `running` row gets reaped to
            // `abandoned` by the startup sweep anyway.
            "running" => RunStatus::Dispatched,
            "success" => RunStatus::Succeeded,
            _ => return None,
        })
    }

    /// Whether a run in this state still occupies the single-flight slot.
    ///
    /// This is the predicate behind both `try_begin_run`'s duplicate
    /// check and `count_active_runs` (the keep-awake signal). They MUST
    /// agree — if `count_active_runs` counted a state `try_begin_run`
    /// does not, a single row could both block dispatch and keep the
    /// machine awake forever.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, RunStatus::Claimed | RunStatus::Dispatched)
    }

    /// Whether the dispatch layer (not the agent) failed, i.e. whether a
    /// retry is safe. Retrying a dispatched run would repeat side effects.
    pub fn is_dispatch_failure(&self) -> bool {
        matches!(self, RunStatus::FailedToDispatch)
    }
}

/// Run record — maps to the `automation_runs` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRun {
    /// Unique run ID.
    pub id: String,
    /// FK to `automations.id`.
    pub automation_id: String,
    /// Execution start timestamp (Unix milliseconds).
    pub started_at: i64,
    /// Execution end timestamp (Unix milliseconds). `None` while in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// Lifecycle state. Serialized as the snake_case wire form.
    pub status: RunStatus,
    /// Task ID generated by this run (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Error message (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    /// The `next_run_at` this run was claimed for. Part of the runId
    /// contract (`automationId + trigger + scheduledAt`): two ticks that
    /// resolve to the same scheduled time are the same run, not two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<i64>,
    /// What caused this run: the scheduler, or a user clicking "run now".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// When the run was claimed (Unix ms). Written synchronously with the
    /// claim; `started_at` is kept for back-compat with existing readers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<i64>,
    /// When the run was handed to the agent (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<i64>,
    /// Dispatch attempts so far, including the current one. Drives backoff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<i64>,
    /// When a transiently-failed dispatch should be retried (Unix ms).
    /// `None` once the run reached a terminal state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,
}

/// Parameters for creating a new automation.
#[derive(Debug, Clone)]
pub struct CreateAutomationParams {
    pub name: String,
    pub prompt: String,
    pub schedule_type: ScheduleType,
    pub rrule: Option<String>,
    pub scheduled_at: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub cwds: Option<String>,
    pub connector_ids_json: Option<String>,
    pub expert_id: Option<String>,
    pub model_id: Option<String>,
    /// Conversation to anchor results to (see [`Automation::anchor_session_id`]).
    pub anchor_session_id: Option<String>,
}

impl Default for CreateAutomationParams {
    fn default() -> Self {
        Self {
            name: "Untitled Automation".to_string(),
            prompt: String::new(),
            schedule_type: ScheduleType::Recurring,
            rrule: None,
            scheduled_at: None,
            valid_from: None,
            valid_until: None,
            cwds: None,
            connector_ids_json: None,
            expert_id: None,
            model_id: None,
            anchor_session_id: None,
        }
    }
}

impl Automation {
    /// Create a new automation with sensible defaults.
    pub fn new(params: CreateAutomationParams) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: params.name,
            prompt: params.prompt,
            schedule_type: params.schedule_type,
            rrule: params.rrule,
            scheduled_at: params.scheduled_at,
            status: AutomationStatus::Active,
            valid_from: params.valid_from,
            valid_until: params.valid_until,
            cwds: params.cwds,
            connector_ids_json: params.connector_ids_json,
            expert_id: params.expert_id,
            model_id: params.model_id,
            anchor_session_id: params.anchor_session_id,
            created_at: now,
            updated_at: now,
        }
    }

    /// Returns `true` if the automation is active.
    pub fn is_active(&self) -> bool {
        self.status == AutomationStatus::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::traits::AgentReply;

    /// 造一个"agent 报错误"的 reply。`total_turns` 是判据的承重项。
    fn errored_reply(total_turns: u32, last_error: &str) -> AgentReply {
        AgentReply {
            text: "✅ 已执行完毕（本轮未生成可见文本总结）。".into(),
            message_count: 1,
            finish_reason: Some("error".into()),
            last_error: Some(last_error.into()),
            interrupted: true,
            total_turns,
        }
    }

    /// 正常完成的 reply。
    fn ok_reply(total_turns: u32) -> AgentReply {
        AgentReply {
            text: "done".into(),
            message_count: 4,
            finish_reason: Some("stop".into()),
            last_error: None,
            interrupted: false,
            total_turns,
        }
    }

    /// 首轮 LLM 挂掉且原因是世界状态问题 → 可安全重试。
    ///
    /// 这是整个退避机制存在的唯一入口。若这条判据不成立，`MAX_DISPATCH_ATTEMPTS`
    /// 和 `backoff_delay_ms` 永远不会被触发。
    #[test]
    fn test_first_turn_transient_failure_is_retryable() {
        let out = dispatch_outcome(&errored_reply(1, "API error 503: service unavailable"));
        assert_eq!(
            out,
            DispatchOutcome::DispatchFailed { retryable: true },
            "首轮失败 = 零工具副作用，503 属世界状态问题，必须可重试"
        );
        assert!(out.is_retry_scheduled());
    }

    /// 跑过工具后失败 → **绝不重试**，哪怕错误文本是 503。
    ///
    /// 这是本函数最重要的一条：重放会重复已发生的副作用，代价远大于
    /// 漏掉一次重试。
    #[test]
    fn test_failure_after_tools_is_never_retryable() {
        for turns in [2u32, 3, 17] {
            let out = dispatch_outcome(&errored_reply(turns, "API error 503: service unavailable"));
            assert_eq!(
                out,
                DispatchOutcome::Failed { retryable: false },
                "第 {turns} 轮失败时工具可能已执行，无论错误多像瞬时故障都不得重试"
            );
            assert!(!out.is_retry_scheduled());
        }
    }

    /// 首轮失败但原因是本地配置 → 不可重试，等用户改配置。
    #[test]
    fn test_first_turn_permanent_failure_not_retryable() {
        let out = dispatch_outcome(&errored_reply(1, "No LLM provider has been configured"));
        assert_eq!(out, DispatchOutcome::DispatchFailed { retryable: false });
        assert!(!out.is_retry_scheduled());
    }

    /// 正常完成不得被误判成失败。
    ///
    /// 回归防护：`interrupted` 与 `total_turns` 都不参与判定，只有
    /// `finish_reason == "error"` 算数。否则一次被用户取消的正常 run
    /// 会被记成失败，而一次真失败会被记成成功。
    #[test]
    fn test_success_is_not_misread_as_failure() {
        assert_eq!(dispatch_outcome(&ok_reply(5)), DispatchOutcome::Succeeded);
        // 中断但不是 error：仍按完成记账。
        let mut r = ok_reply(5);
        r.interrupted = true;
        r.finish_reason = Some("interrupted".into());
        assert_eq!(dispatch_outcome(&r), DispatchOutcome::Succeeded);
    }

    /// 瞬时判定必须能认出真实 provider 故障的字样。
    #[test]
    fn test_transient_text_covers_real_provider_failures() {
        for t in [
            "API error 429: rate limit exceeded",
            "API error 503: service unavailable",
            "API error 502: bad gateway",
            "API error 504: gateway timeout",
            "request timeout after 30s",
            "connection reset by peer",
            "error sending request: connection closed before message completed",
        ] {
            let out = dispatch_outcome(&errored_reply(1, t));
            assert!(
                matches!(out, DispatchOutcome::DispatchFailed { retryable: true }),
                "「{t}」应判为可重试"
            );
        }
    }

    /// 认证/配置类错误绝不能被误判成瞬时故障——
    /// 否则会对着一个坏掉的 key 重试三次，纯粹浪费。
    #[test]
    fn test_permanent_text_not_misread_as_transient() {
        for t in [
            "Authentication failed: invalid api key",
            "No LLM provider has been configured",
            "invalid tool call arguments",
        ] {
            let out = dispatch_outcome(&errored_reply(1, t));
            assert!(
                matches!(out, DispatchOutcome::DispatchFailed { retryable: false }),
                "「{t}」不该被重试"
            );
        }
    }

    #[test]
    fn test_automation_new_defaults() {
        let params = CreateAutomationParams {
            name: "Daily Brief".into(),
            prompt: "Summarize today's news".into(),
            schedule_type: ScheduleType::Recurring,
            rrule: Some("0 8 * * *".into()),
            ..Default::default()
        };
        let auto = Automation::new(params);
        assert_eq!(auto.name, "Daily Brief");
        assert_eq!(auto.status, AutomationStatus::Active);
        assert!(auto.is_active());
        assert_eq!(auto.schedule_type, ScheduleType::Recurring);
    }

    #[test]
    fn test_schedule_type_display() {
        assert_eq!(ScheduleType::Recurring.to_string(), "recurring");
        assert_eq!(ScheduleType::Once.to_string(), "once");
    }

    #[test]
    fn test_automation_status_display() {
        assert_eq!(AutomationStatus::Active.to_string(), "ACTIVE");
        assert_eq!(AutomationStatus::Paused.to_string(), "PAUSED");
    }
}
