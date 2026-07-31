//! Automation scheduler — background tick loop that fires due automations.
//!
//! Ticks every 30 seconds, walks every ACTIVE automation, computes the next
//! firing time, and dispatches any automation whose `next_run_at` has elapsed.
//! Dispatch is fire-and-forget — we don't block the tick loop on a slow run.
//!
//!
//!
//! The scheduler is decoupled from the execution layer via a callback:
//! the caller provides a `TriggerFn` that receives the automation and
//! is responsible for spawning the actual agent run. This keeps the
//! scheduler crate free of `hermix-agent` / `hermix-server` dependencies.

use std::sync::Arc;
use std::time::Duration;

use crate::{Automation, AutomationService, AutomationStatus, ScheduleType};

/// Callback invoked when an automation is due to fire.
/// The callback receives the automation definition and should:
/// 1. Record a run start
/// 2. Spawn the agent execution
/// 3. Record the run result when complete
pub type TriggerFn = Arc<dyn Fn(Automation) + Send + Sync>;

/// Background scheduler that ticks every 30 seconds and fires due automations.
pub struct AutomationScheduler {
    shutdown: tokio::sync::watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

impl AutomationScheduler {
    /// Start the background tick loop.
    /// - `service`: the automation service for querying + updating state
    /// - `trigger`: callback invoked for each due automation
    /// - `tick_interval`: how often to check for due automations (default 30s)
    pub fn start(
        service: Arc<AutomationService>,
        trigger: TriggerFn,
        tick_interval: Duration,
    ) -> Arc<Self> {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(tick_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if let Err(e) = tick_once(&service, &trigger).await {
                            tracing::warn!(error=%e, "automation scheduler tick failed");
                        }
                    }
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                }
            }
            tracing::info!("automation scheduler shut down");
        });
        Arc::new(Self {
            shutdown: tx,
            _handle: handle,
        })
    }

    /// Signal the background loop to exit. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// Process one tick: check all active automations and fire any that are due.
async fn tick_once(
    service: &AutomationService,
    trigger: &TriggerFn,
) -> Result<(), String> {
    let automations = service
        .list(Some(AutomationStatus::Active))
        .map_err(|e| e.to_string())?;

    let now = chrono::Utc::now().timestamp_millis();

    for auto in automations {
        // Get current runtime state
        let state = match service.get_runtime_state(&auto.id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    automation_id = %auto.id,
                    error = %e,
                    "automation tick: failed to get runtime state, skipping"
                );
                continue;
            }
        };

        // Check if this automation should fire now
        let should_fire = match auto.schedule_type {
            ScheduleType::Recurring => {
                // Fire if next_run_at is set and <= now, and we haven't
                // already fired since the last run
                match state.next_run_at {
                    Some(next) if next <= now => {
                        match state.last_run_at {
                            Some(last) => last < next,
                            None => true,
                        }
                    }
                    _ => false,
                }
            }
            ScheduleType::Once => {
                // Fire if scheduled_at is set and <= now, and we haven't fired yet
                match state.next_run_at {
                    Some(next) if next <= now => state.last_run_at.is_none(),
                    _ => false,
                }
            }
        };

        if should_fire {
            tracing::info!(
                automation_id = %auto.id,
                automation_name = %auto.name,
                "automation tick: firing automation"
            );
            // Invoke the trigger callback
            trigger(auto.clone());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutomationRepository, CreateAutomationParams};
    use std::sync::Mutex;

    fn service() -> (Arc<AutomationService>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(AutomationRepository::new(tmp.path().join("test.db")));
        repo.ensure_schema().unwrap();
        let svc = Arc::new(AutomationService::new(repo));
        (svc, tmp)
    }

    #[tokio::test]
    async fn test_scheduler_does_not_fire_paused() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("* * * * * *".into()), // Every second
                ..Default::default()
            })
            .unwrap();
        svc.pause(&auto.id).unwrap();

        let fired = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let trigger: TriggerFn = Arc::new(move |a| {
            fired_clone.lock().unwrap().push(a.id);
        });

        // Run one tick
        tick_once(&svc, &trigger).await.unwrap();

        assert_eq!(fired.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_scheduler_fires_active_automation() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Test".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Once,
                scheduled_at: Some(
                    chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
                ..Default::default()
            })
            .unwrap();

        let fired = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let trigger: TriggerFn = Arc::new(move |a| {
            fired_clone.lock().unwrap().push(a.id);
        });

        // Run one tick — should fire since scheduled_at is now or past
        tick_once(&svc, &trigger).await.unwrap();

        let fired_list = fired.lock().unwrap();
        assert_eq!(fired_list.len(), 1);
        assert_eq!(fired_list[0], auto.id);
    }

    #[tokio::test]
    async fn test_scheduler_does_not_fire_future_once() {
        let (svc, _tmp) = service();
        let future = (chrono::Utc::now() + chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _auto = svc
            .create(CreateAutomationParams {
                name: "Future".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Once,
                scheduled_at: Some(future),
                ..Default::default()
            })
            .unwrap();

        let fired = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let trigger: TriggerFn = Arc::new(move |a| {
            fired_clone.lock().unwrap().push(a.id);
        });

        tick_once(&svc, &trigger).await.unwrap();

        assert_eq!(fired.lock().unwrap().len(), 0);
    }
}
