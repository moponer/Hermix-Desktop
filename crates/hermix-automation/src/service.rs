//! Automation lifecycle service — create/query/update/pause/resume + trigger.
//!
//! Wraps [`AutomationRepository`] with business logic:
//! - Schedule validation (cron expression / one-time timestamp)
//! - Runtime state maintenance (last/next run, run count)
//! - Run history recording
//!
//!

use std::sync::Arc;

use crate::{
    compose_run_id, Automation, AutomationRepository, AutomationRepositoryError, AutomationRun,
    AutomationRuntimeState, AutomationStatus, CreateAutomationParams, RunStatus, RunTrigger,
    ScheduleType,
};

/// Total dispatch attempts for one slot before it is given up on.
///
/// Counts the first try, so `1` means "no retry at all". Kept low on
/// purpose: a daily job that keeps failing should surface its error
/// within the same wake-up rather than retrying for hours.
pub const MAX_DISPATCH_ATTEMPTS: i64 = 3;

/// Backoff before attempt `n` (1-based), in milliseconds.
///
/// Exponential because these failures are sustained — a provider outage
/// or a rate limit does not clear in a fixed number of seconds, and a
/// flat interval would double the request rate precisely when the far
/// side is already struggling. Capped so a slot is never deferred past
/// the next scheduled time by more than one tick interval's worth of
/// patience.
pub fn backoff_delay_ms(attempt: i64) -> i64 {
    const BASE_MS: i64 = 30_000;
    const CAP_MS: i64 = 10 * 60_000;
    let exp = (attempt - 1).clamp(0, 16) as u32;
    BASE_MS.saturating_mul(1i64 << exp).min(CAP_MS)
}

#[derive(Debug, thiserror::Error)]
pub enum AutomationServiceError {
    #[error("Repository error: {0}")]
    Repository(#[from] AutomationRepositoryError),
    #[error("Automation not found: {0}")]
    NotFound(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

/// Lifecycle management service for automations.
pub struct AutomationService {
    repo: Arc<AutomationRepository>,
}

impl AutomationService {
    pub fn new(repo: Arc<AutomationRepository>) -> Self {
        Self { repo }
    }

    /// Create a new automation. Validates schedule type + expression.
    pub fn create(&self, params: CreateAutomationParams) -> Result<Automation, AutomationServiceError> {
        if params.name.trim().is_empty() {
            return Err(AutomationServiceError::Validation("name cannot be empty".into()));
        }
        if params.prompt.trim().is_empty() {
            return Err(AutomationServiceError::Validation("prompt cannot be empty".into()));
        }

        // Validate schedule
        match params.schedule_type {
            ScheduleType::Recurring => {
                if let Some(ref rrule) = params.rrule {
                    if normalize_cron_expr(rrule).parse::<cron::Schedule>().is_err() {
                        return Err(AutomationServiceError::Validation(format!(
                            "Invalid cron expression: '{}'", rrule
                        )));
                    }
                } else {
                    return Err(AutomationServiceError::Validation(
                        "Recurring schedule requires 'rrule' field".into(),
                    ));
                }
            }
            ScheduleType::Once => {
                if params.scheduled_at.is_none() {
                    return Err(AutomationServiceError::Validation(
                        "Once schedule requires 'scheduled_at' field".into(),
                    ));
                }
            }
        }

        let auto = Automation::new(params);
        self.repo.create(&auto)?;

        // Compute initial next_run_at
        if let Some(next) = self.compute_next_run(&auto) {
            self.repo.update_runtime_state(&auto.id, None, Some(next), 0)?;
        }

        Ok(auto)
    }

    /// Get an automation by ID.
    pub fn get(&self, id: &str) -> Result<Automation, AutomationServiceError> {
        self.repo.get(id).map_err(|e| match e {
            AutomationRepositoryError::NotFound(id) => AutomationServiceError::NotFound(id),
            other => AutomationServiceError::Repository(other),
        })
    }

    /// List all automations, optionally filtered by status.
    pub fn list(&self, status: Option<AutomationStatus>) -> Result<Vec<Automation>, AutomationServiceError> {
        self.repo.list(status).map_err(Into::into)
    }

    /// Pause an automation (set status to PAUSED).
    pub fn pause(&self, id: &str) -> Result<Automation, AutomationServiceError> {
        self.repo.update_status(id, AutomationStatus::Paused)?;
        self.get(id)
    }

    /// Resume an automation (set status to ACTIVE).
    pub fn resume(&self, id: &str) -> Result<Automation, AutomationServiceError> {
        self.repo.update_status(id, AutomationStatus::Active)?;
        // Recompute next_run_at
        let auto = self.get(id)?;
        if let Some(next) = self.compute_next_run(&auto) {
            let state = self.repo.get_runtime_state(id)?;
            self.repo.update_runtime_state(id, state.last_run_at, Some(next), state.run_count)?;
        }
        self.get(id)
    }

    /// Delete an automation (cascades to runtime state + runs).
    pub fn delete(&self, id: &str) -> Result<(), AutomationServiceError> {
        self.repo.delete(id).map_err(|e| match e {
            AutomationRepositoryError::NotFound(id) => AutomationServiceError::NotFound(id),
            other => AutomationServiceError::Repository(other),
        })
    }

    /// Get the runtime state for an automation.
    pub fn get_runtime_state(&self, id: &str) -> Result<AutomationRuntimeState, AutomationServiceError> {
        self.repo.get_runtime_state(id).map_err(|e| match e {
            AutomationRepositoryError::NotFound(id) => AutomationServiceError::NotFound(id),
            other => AutomationServiceError::Repository(other),
        })
    }

    /// List run history for an automation.
    pub fn list_runs(&self, id: &str, limit: usize) -> Result<Vec<AutomationRun>, AutomationServiceError> {
        self.repo.list_runs(id, limit).map_err(Into::into)
    }

    /// Number of automation runs currently in flight.
    ///
    /// Drives the desktop "keep the machine awake while work is running"
    /// signal: a background automation has no streaming session, so the
    /// front-end cannot infer activity from its own session state.
    pub fn count_active_runs(&self) -> Result<u64, AutomationServiceError> {
        self.repo.count_active_runs().map_err(Into::into)
    }

    /// Reap runs abandoned by a previous process. Call once at startup.
    ///
    /// Without this, a crash between claim and dispatch leaves a permanent
    /// in-flight row that both blocks the automation forever and holds the
    /// keep-awake signal down — the machine would never sleep again, with
    /// nothing in the UI to explain it.
    pub fn reap_abandoned_runs(&self) -> Result<usize, AutomationServiceError> {
        let n = self.repo.reap_abandoned_runs()?;
        if n > 0 {
            tracing::warn!(
                reaped = n,
                "automation: reaped runs abandoned by a previous process"
            );
        }
        Ok(n)
    }

    /// Claim the single-flight slot for a run, synchronously.
    ///
    /// Both entry points go through here — the scheduler tick and the
    /// manual "run now" button — so neither can bypass the other's
    /// exclusion. Returns the run id on success, or `None` when the claim
    /// was declined (already in flight, `max_runs` exhausted, or
    /// `valid_until` passed). A declined claim records a `skipped` run so
    /// the decision is visible in history rather than silently swallowed.
    ///
    /// **This must be called before spawning the async work.** The
    /// original code recorded the run from inside the spawned task, so
    /// `trigger()` returned with no row written and `last_run_at` still
    /// stale — a slow spawn meant the next 30s tick judged the automation
    /// due again and fired it a second time. Writing the claim inline
    /// closes that window.
    pub fn try_begin_run(
        &self,
        automation_id: &str,
        trigger: RunTrigger,
        scheduled_at: Option<i64>,
    ) -> Result<Option<String>, AutomationServiceError> {
        let run_id = compose_run_id(automation_id, trigger, scheduled_at);

        // Dedupe first: the same (automation, trigger, slot) triple is one
        // run no matter how many ticks or restarts land on it.
        if self.repo.run_exists(&run_id)? {
            return Ok(None);
        }

        let auto = self.get(automation_id)?;

        // Single-flight: another run of this automation still holds the slot.
        if self.repo.count_in_flight(automation_id)? > 0 {
            self.record_skipped(&run_id, &auto, trigger, scheduled_at, "another run is in flight")?;
            return Ok(None);
        }

        // valid_until passed. (`max_runs` is **not** checked here: the
        // field only exists on the dashboard's `AutomationTask`, not on
        // `hermix_automation::Automation` — the two models are separate
        // and the scheduler reads the latter. Adding a cap means adding
        // the column to this table, not reading the other one.)
        if let Some(ref until) = auto.valid_until {
            if let Ok(until_dt) = chrono::DateTime::parse_from_rfc3339(until) {
                if chrono::Utc::now() > until_dt.with_timezone(&chrono::Utc) {
                    self.record_skipped(&run_id, &auto, trigger, scheduled_at, "schedule has ended")?;
                    return Ok(None);
                }
            }
        }

        let now = chrono::Utc::now().timestamp_millis();
        let run = AutomationRun {
            id: run_id.clone(),
            automation_id: auto.id.clone(),
            started_at: now,
            finished_at: None,
            status: RunStatus::Claimed,
            task_id: None,
            error_text: None,
            scheduled_at,
            trigger: Some(trigger.as_str().to_string()),
            claimed_at: Some(now),
            dispatched_at: None,
            attempts: Some(1),
            retry_at: None,
        };
        self.repo.create_run(&run)?;
        Ok(Some(run_id))
    }

    /// Write a terminal `skipped` row for a declined claim.
    ///
    /// `skipped` is not a failure: it records "we took the slot, then
    /// chose not to run", so the UI can show why a due automation did
    /// nothing without painting it red.
    fn record_skipped(
        &self,
        run_id: &str,
        auto: &Automation,
        trigger: RunTrigger,
        scheduled_at: Option<i64>,
        reason: &str,
    ) -> Result<(), AutomationServiceError> {
        let now = chrono::Utc::now().timestamp_millis();
        self.repo.create_run(&AutomationRun {
            id: run_id.to_string(),
            automation_id: auto.id.clone(),
            started_at: now,
            finished_at: Some(now),
            status: RunStatus::Skipped,
            task_id: None,
            error_text: Some(reason.to_string()),
            scheduled_at,
            trigger: Some(trigger.as_str().to_string()),
            claimed_at: Some(now),
            dispatched_at: None,
            attempts: Some(0),
            retry_at: None,
        })?;
        tracing::debug!(
            automation_id = %auto.id,
            run_id = %run_id,
            reason = %reason,
            "automation: claim declined, run recorded as skipped"
        );
        Ok(())
    }

    /// Mark a claimed run as handed to the agent.
    pub fn mark_dispatched(&self, run_id: &str) -> Result<(), AutomationServiceError> {
        self.repo.mark_run_dispatched(run_id).map_err(Into::into)
    }

    /// Record the completion of an agent run.
    pub fn record_run_finish(
        &self,
        run_id: &str,
        status: RunStatus,
        task_id: Option<&str>,
        error_text: Option<&str>,
    ) -> Result<(), AutomationServiceError> {
        let now = chrono::Utc::now().timestamp_millis();
        self.repo.update_run(run_id, now, status, task_id, error_text)?;
        Ok(())
    }

    /// Schedule a retry for a run whose **dispatch** failed transiently.
    ///
    /// Only ever call this for dispatch-layer failures. An agent that ran
    /// and failed may already have had side effects (sent messages, wrote
    /// files); retrying it would repeat them.
    pub fn schedule_dispatch_retry(
        &self,
        run_id: &str,
        automation_id: &str,
        error_text: &str,
    ) -> Result<Option<i64>, AutomationServiceError> {
        let streak = self.repo.consecutive_dispatch_failures(automation_id)?;
        if streak >= MAX_DISPATCH_ATTEMPTS {
            return Ok(None);
        }
        let attempts = streak + 1;
        let retry_at = chrono::Utc::now().timestamp_millis() + backoff_delay_ms(attempts);
        self.repo.schedule_dispatch_retry(run_id, retry_at, attempts, error_text)?;
        Ok(Some(retry_at))
    }

    /// Update runtime state after a run completes.
    pub fn update_after_run(
        &self,
        automation_id: &str,
    ) -> Result<(), AutomationServiceError> {
        let auto = self.get(automation_id)?;
        let state = self.repo.get_runtime_state(automation_id)?;
        let now = chrono::Utc::now().timestamp_millis();
        let next_run = self.compute_next_run(&auto);
        self.repo.update_runtime_state(
            automation_id,
            Some(now),
            next_run,
            state.run_count + 1,
        )?;
        Ok(())
    }

    /// Hold the schedule in place after a dispatch failure, so the retry
    /// is not silently skipped by the next tick.
    ///
    /// The backoff contract lives in `scheduler.rs::should_fire`:
    /// `next_run_at` must move to the retry time **and `last_run_at` must
    /// stay put** — `should_fire` reads `last_run_at < next_run_at`, so
    /// touching `last_run_at` would make the retry never fire. `run_count`
    /// is deliberately not incremented: a dispatch that never reached the
    /// agent is not an execution.
    pub fn defer_for_retry(
        &self,
        automation_id: &str,
        retry_at: i64,
    ) -> Result<(), AutomationServiceError> {
        let state = self.repo.get_runtime_state(automation_id)?;
        self.repo.update_runtime_state(
            automation_id,
            state.last_run_at, // unchanged — see doc comment
            Some(retry_at),
            state.run_count,
        )?;
        Ok(())
    }

    /// Compute the next run timestamp for an automation.
    /// Returns None if the automation is paused, expired, or has no valid schedule.
    pub fn compute_next_run(&self, auto: &Automation) -> Option<i64> {
        if !auto.is_active() {
            return None;
        }

        let now = chrono::Utc::now();

        // Check valid_from / valid_until
        if let Some(ref from) = auto.valid_from {
            if let Ok(from_dt) = chrono::DateTime::parse_from_rfc3339(from) {
                if now < from_dt.with_timezone(&chrono::Utc) {
                    // Not yet valid — return the first valid run time
                    // For simplicity, just return None (will be recomputed on next tick)
                    return None;
                }
            }
        }
        if let Some(ref until) = auto.valid_until {
            if let Ok(until_dt) = chrono::DateTime::parse_from_rfc3339(until) {
                if now > until_dt.with_timezone(&chrono::Utc) {
                    return None; // Expired
                }
            }
        }

        match auto.schedule_type {
            ScheduleType::Recurring => {
                let rrule = auto.rrule.as_ref()?;
                let normalized = normalize_cron_expr(rrule);
                let sched = normalized.parse::<cron::Schedule>().ok()?;
                let next = sched.after(&now).next()?;
                Some(next.timestamp_millis())
            }
            ScheduleType::Once => {
                // For one-time schedules, always return the scheduled timestamp.
                // The scheduler decides whether to fire based on whether
                // last_run_at is None (hasn't been fired yet).
                let scheduled_str = auto.scheduled_at.as_ref()?;
                let dt = chrono::DateTime::parse_from_rfc3339(scheduled_str).ok()?;
                Some(dt.timestamp_millis())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cron helpers (mirrors dashboard/automation.rs)
// ---------------------------------------------------------------------------

/// Normalize a user-supplied cron expression to the 7-field format
/// (`sec min hour dom month dow year`) required by the `cron` crate.
/// 5-field → `0 <expr> *`; 6-field → `<expr> *`; 7-field → as-is.
pub(crate) fn normalize_cron_expr(expr: &str) -> String {
    let parts: Vec<&str> = expr.trim().split_whitespace().collect();
    match parts.len() {
        5 => format!("0 {} *", expr.trim()),
        6 => format!("{} *", expr.trim()),
        7 => expr.trim().to_string(),
        _ => expr.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (AutomationService, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(AutomationRepository::new(tmp.path().join("test.db")));
        repo.ensure_schema().unwrap();
        let svc = AutomationService::new(repo);
        (svc, tmp)
    }

    #[test]
    fn test_create_automation() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Daily Brief".into(),
                prompt: "Summarize news".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(auto.name, "Daily Brief");
        assert_eq!(auto.status, AutomationStatus::Active);
    }

    #[test]
    fn test_create_empty_name_rejected() {
        let (svc, _tmp) = service();
        let err = svc
            .create(CreateAutomationParams {
                name: "  ".into(),
                prompt: "Test".into(),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, AutomationServiceError::Validation(_)));
    }

    #[test]
    fn test_create_invalid_cron_rejected() {
        let (svc, _tmp) = service();
        let err = svc
            .create(CreateAutomationParams {
                name: "Bad".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("not-a-cron".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, AutomationServiceError::Validation(_)));
    }

    #[test]
    fn test_create_once_without_scheduled_at_rejected() {
        let (svc, _tmp) = service();
        let err = svc
            .create(CreateAutomationParams {
                name: "Once".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Once,
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, AutomationServiceError::Validation(_)));
    }

    #[test]
    fn test_pause_and_resume() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let paused = svc.pause(&auto.id).unwrap();
        assert_eq!(paused.status, AutomationStatus::Paused);

        let resumed = svc.resume(&auto.id).unwrap();
        assert_eq!(resumed.status, AutomationStatus::Active);
    }

    #[test]
    fn test_delete() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();
        svc.delete(&auto.id).unwrap();
        let err = svc.get(&auto.id).unwrap_err();
        assert!(matches!(err, AutomationServiceError::NotFound(_)));
    }

    #[test]
    fn test_record_run() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = svc
            .try_begin_run(&auto.id, RunTrigger::Manual, None)
            .unwrap()
            .expect("manual claim should succeed");
        svc.record_run_finish(&run_id, RunStatus::Succeeded, Some("task-123"), None)
            .unwrap();
        svc.update_after_run(&auto.id).unwrap();

        let state = svc.get_runtime_state(&auto.id).unwrap();
        assert_eq!(state.run_count, 1);
        assert!(state.last_run_at.is_some());

        let runs = svc.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert_eq!(runs[0].task_id.as_deref(), Some("task-123"));
    }

    /// 同一 slot 二次 claim 必须被拒——这是"tick 跑两次"的直接防线。
    #[test]
    fn test_try_begin_run_dedupes_same_slot() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let slot = 1_700_000_000_000i64;
        let first = svc
            .try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot))
            .unwrap()
            .expect("首次 claim 应成功");
        assert!(
            svc.try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot)).unwrap().is_none(),
            "同一 slot 不得二次 claim"
        );

        // 收尾后才轮到下一个 slot——单飞期内任何 claim 都会被拒，
        // 那是另一个不变量（见 test_try_begin_run_respects_single_flight）。
        svc.record_run_finish(&first, RunStatus::Failed, None, Some("boom"))
            .unwrap();
        assert!(
            svc.try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot + 60_000)).unwrap().is_some(),
            "前一次收尾后，不同 slot 应可 claim"
        );
    }

    /// 手动触发两次必须都能 claim——去重键是给"调度槽位"用的，
    /// 手动点击每次都是一个独立意图。
    ///
    /// 回归防护：早期 `compose_run_id` 对 `scheduled_at = None` 返回
    /// 恒定的 `{automation}:manual`，于是当天第二次"立即运行"被当成
    /// 重复槽位静默丢弃——用户点了按钮，什么都没发生，也没有报错。
    /// 单飞不是这个场景的防线（那次 run 已收尾），所以这条只能是独立
    /// 断言。
    #[test]
    fn test_manual_triggers_are_not_deduped_against_each_other() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let first = svc
            .try_begin_run(&auto.id, RunTrigger::Manual, None)
            .unwrap()
            .expect("首次手动触发应成功");
        svc.record_run_finish(&first, RunStatus::Succeeded, None, None)
            .unwrap();

        let second = svc.try_begin_run(&auto.id, RunTrigger::Manual, None);
        assert!(
            second.unwrap().is_some(),
            "前一次已收尾，第二次手动触发必须被允许"
        );
    }

    /// 单飞：上一次 run 未收尾时，任何 claim 都被拒并记 skipped。
    #[test]
    fn test_try_begin_run_respects_single_flight() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        svc.try_begin_run(&auto.id, RunTrigger::Scheduled, Some(1_000)).unwrap();
        assert!(
            svc.try_begin_run(&auto.id, RunTrigger::Scheduled, Some(2_000)).unwrap().is_none(),
            "在途 run 期间不得再 claim"
        );
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        let skipped = runs.iter().find(|r| r.status == RunStatus::Skipped);
        assert!(
            skipped.is_some(),
            "被拒的 claim 要留下 skipped 记录，让 UI 知道到点为何没跑"
        );
    }

    /// `valid_until` 已过 → 记 skipped，不派发。
    #[test]
    fn test_try_begin_run_skips_after_valid_until() {
        let (svc, _tmp) = service();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let auto = svc
            .create(CreateAutomationParams {
                name: "Expired".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                valid_until: Some(past),
                ..Default::default()
            })
            .unwrap();

        assert!(svc.try_begin_run(&auto.id, RunTrigger::Manual, None).unwrap().is_none());
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Skipped);
        // skipped 不是失败：不该占住单飞槽。
        assert_eq!(svc.count_active_runs().unwrap(), 0);
    }

    /// 退避不得超过上限次数。
    #[test]
    fn test_schedule_dispatch_retry_gives_up_at_cap() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Flaky".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        for i in 0..MAX_DISPATCH_ATTEMPTS {
            let slot = 1_700_000_000_000i64 + i * 60_000;
            let run_id = svc
                .try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot))
                .unwrap()
                .unwrap();
            let retry_at = svc
                .schedule_dispatch_retry(&run_id, &auto.id, "503")
                .unwrap();
            assert!(retry_at.is_some(), "第 {} 次应仍可重试", i + 1);
        }
        // 超过上限 → None，调用方转为终态失败。
        let slot = 1_700_000_000_000i64 + MAX_DISPATCH_ATTEMPTS * 60_000;
        let run_id = svc
            .try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot))
            .unwrap()
            .unwrap();
        assert!(svc.schedule_dispatch_retry(&run_id, &auto.id, "503").unwrap().is_none());
    }

    /// 退避不得动 `last_run_at`——`should_fire` 靠它判重。
    #[test]
    fn test_defer_for_retry_preserves_last_run_at() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = svc.try_begin_run(&auto.id, RunTrigger::Manual, None).unwrap().unwrap();
        svc.record_run_finish(&run_id, RunStatus::FailedToDispatch, None, Some("503"))
            .unwrap();
        svc.update_after_run(&auto.id).unwrap();
        let before = svc.get_runtime_state(&auto.id).unwrap().last_run_at;

        let retry_at = chrono::Utc::now().timestamp_millis() + 30_000;
        svc.defer_for_retry(&auto.id, retry_at).unwrap();

        let after = svc.get_runtime_state(&auto.id).unwrap();
        assert_eq!(after.last_run_at, before, "退避不得改 last_run_at，否则重试永不触发");
        assert_eq!(after.next_run_at, Some(retry_at));
        assert_eq!(after.run_count, 1, "派发未达 agent，不该计入执行次数");
    }

    #[test]
    fn test_compute_next_run_recurring() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();
        let next = svc.compute_next_run(&auto);
        assert!(next.is_some());
        assert!(next.unwrap() > chrono::Utc::now().timestamp_millis());
    }

    #[test]
    fn test_compute_next_run_paused() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();
        svc.pause(&auto.id).unwrap();
        let paused_auto = svc.get(&auto.id).unwrap();
        let next = svc.compute_next_run(&paused_auto);
        assert!(next.is_none());
    }

    #[test]
    fn test_compute_next_run_once() {
        let (svc, _tmp) = service();
        let future = (chrono::Utc::now() + chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let auto = svc
            .create(CreateAutomationParams {
                name: "Once".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Once,
                scheduled_at: Some(future),
                ..Default::default()
            })
            .unwrap();
        let next = svc.compute_next_run(&auto);
        assert!(next.is_some());
    }

    #[test]
    fn test_compute_next_run_once_past() {
        // A past one-time schedule still returns its timestamp — the
        // scheduler decides whether to fire based on last_run_at.
        let (svc, _tmp) = service();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let auto = svc
            .create(CreateAutomationParams {
                name: "Once".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Once,
                scheduled_at: Some(past),
                ..Default::default()
            })
            .unwrap();
        let next = svc.compute_next_run(&auto);
        assert!(next.is_some()); // Returns the timestamp even if past
    }
}
