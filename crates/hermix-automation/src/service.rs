//! Automation lifecycle service — create/query/update/pause/resume + trigger.
//!
//! Wraps [`AutomationRepository`] with business logic:
//! - Schedule validation (cron expression / one-time timestamp)
//! - Runtime state maintenance (last/next run, run count)
//! - Run history recording
//!
//! (Phase 5 — P5-3, P5-4)

use std::sync::Arc;

use crate::{
    Automation, AutomationRepository, AutomationRepositoryError, AutomationRun,
    AutomationRuntimeState, AutomationStatus, CreateAutomationParams, ScheduleType,
};

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

    /// Record the start of a run. Returns the run ID.
    pub fn record_run_start(&self, automation_id: &str) -> Result<String, AutomationServiceError> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let run = AutomationRun {
            id: run_id.clone(),
            automation_id: automation_id.to_string(),
            started_at: now,
            finished_at: None,
            status: "running".to_string(),
            task_id: None,
            error_text: None,
        };
        self.repo.create_run(&run)?;
        Ok(run_id)
    }

    /// Record the completion of a run.
    pub fn record_run_finish(
        &self,
        run_id: &str,
        status: &str,
        task_id: Option<&str>,
        error_text: Option<&str>,
    ) -> Result<(), AutomationServiceError> {
        let now = chrono::Utc::now().timestamp_millis();
        self.repo.update_run(run_id, now, status, task_id, error_text)?;

        // Update runtime state: increment run_count, set last_run_at, recompute next_run_at
        // We need to find the automation_id from the run. Since we don't have a
        // get_run method, we'll look it up from the automation's runs.
        // Actually, we should add a get_run method, but for now we'll update
        // the runtime state from the caller's perspective.
        Ok(())
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

        let run_id = svc.record_run_start(&auto.id).unwrap();
        svc.record_run_finish(&run_id, "success", Some("task-123"), None)
            .unwrap();
        svc.update_after_run(&auto.id).unwrap();

        let state = svc.get_runtime_state(&auto.id).unwrap();
        assert_eq!(state.run_count, 1);
        assert!(state.last_run_at.is_some());

        let runs = svc.list_runs(&auto.id, 100).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].task_id.as_deref(), Some("task-123"));
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
