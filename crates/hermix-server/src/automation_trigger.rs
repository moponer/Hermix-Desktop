//! Automation trigger bridge — wires the `hermix-automation` scheduler
//! into the server's unified dispatch path.
//!
//!
//!
//! ## 铁律 DD: HTTP 与自动化同路径
//!
//! When an automation fires, the trigger callback calls
//! `agent_service.send_message()`, which is the **same dispatch path**
//! used by HTTP chat endpoints. Internally, `LocalAgentService::send_message()`
//! runs the `AgentLoop`, which dispatches tool calls through
//! `ToolRegistry::dispatch_async()`. This means:
//!
//! - Automations have the same tool access as interactive chats
//! - Pre-approval checks apply equally
//! - Tool results are processed by the same `tool_result_processor`
//! - Memory / skill / sub-agent hooks all fire
//!
//! ## Flow
//!
//! 1. `AutomationScheduler` ticks → finds due automation → calls `TriggerFn`
//! 2. `TriggerFn` records run start in `automation_runs` table
//! 3. Spawns a tokio task that calls `agent_service.send_message()`
//! 4. On completion: records run result + updates runtime state
//! 5. Emits `AutomationTriggered` event via `OrchestrationEventBus`
//! 6. Optionally creates a Task (if `task_service` is available) and
//!    links it to the automation run

use std::sync::Arc;
use std::time::Duration;

use hermix_automation::{Automation, AutomationScheduler, AutomationService, TriggerFn};
use hermix_core::traits::{AgentOverrides, AgentService};
use hermix_core::TaskContext;

use crate::orchestration_bus::{OrchestrationEvent, OrchestrationEventBus};

/// Handle to the running automation scheduler. Dropping this does NOT
/// stop the scheduler — call `shutdown()` explicitly.
#[derive(Clone)]
pub struct AutomationSchedulerHandle {
    scheduler: Arc<AutomationScheduler>,
}

impl AutomationSchedulerHandle {
    /// Shut down the background tick loop. Idempotent.
    pub fn shutdown(&self) {
        self.scheduler.shutdown();
    }
}

/// Start the automation scheduler with a trigger callback that dispatches
/// through the unified `agent_service.send_message()` path.
/// This wires together:
/// - `automation_service` — for querying due automations + recording runs
/// - `agent_service` — for executing the automation prompt (unified dispatch)
/// - `orchestration_bus` — for emitting `AutomationTriggered` events
/// - `task_service` (optional) — for creating a Task per automation run
/// Returns a handle that can be used to shut down the scheduler.
pub fn start_automation_scheduler(
    automation_service: Arc<AutomationService>,
    agent_service: Arc<dyn AgentService>,
    orchestration_bus: OrchestrationEventBus,
    task_service: Option<Arc<hermix_task::TaskService>>,
) -> AutomationSchedulerHandle {
    let trigger = build_trigger_callback(
        automation_service.clone(),
        agent_service,
        orchestration_bus,
        task_service,
    );

    let scheduler = AutomationScheduler::start(
        automation_service,
        trigger,
        Duration::from_secs(30),
    );

    AutomationSchedulerHandle { scheduler }
}

/// Build the trigger callback that fires when an automation is due.
/// The callback:
/// 1. Records a run start in the `automation_runs` table
/// 2. Spawns a tokio task to execute the automation prompt
/// 3. The spawned task calls `agent_service.send_message()` (unified dispatch)
/// 4. Records the run result and updates runtime state
/// 5. Emits `AutomationTriggered` via the orchestration bus
fn build_trigger_callback(
    automation_service: Arc<AutomationService>,
    agent_service: Arc<dyn AgentService>,
    orchestration_bus: OrchestrationEventBus,
    task_service: Option<Arc<hermix_task::TaskService>>,
) -> TriggerFn {
    Arc::new(move |auto: Automation| {
        let svc = automation_service.clone();
        let agent = agent_service.clone();
        let bus = orchestration_bus.clone();
        let tasks = task_service.clone();

        tokio::spawn(async move {
            execute_automation_run(&auto, &svc, &agent, &bus, tasks.as_deref()).await;
        });
    })
}

/// Execute a single automation run end-to-end.
/// This is the core of the unified dispatch path: it calls
/// `agent_service.send_message()`, which internally runs the full
/// `AgentLoop` with `ToolRegistry::dispatch_async()` — the same path
/// as HTTP chat requests.
async fn execute_automation_run(
    auto: &Automation,
    svc: &AutomationService,
    agent: &Arc<dyn AgentService>,
    bus: &OrchestrationEventBus,
    task_service: Option<&hermix_task::TaskService>,
) {
    // 1. Record run start
    let run_id = match svc.record_run_start(&auto.id) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                automation_id = %auto.id,
                error = %e,
                "automation trigger: failed to record run start"
            );
            return;
        }
    };

    tracing::info!(
        automation_id = %auto.id,
        automation_name = %auto.name,
        run_id = %run_id,
        "automation triggered — dispatching via agent_service.send_message()"
    );

    // 2. Optionally create a Task for this automation run
    let task_id = if let Some(ts) = task_service {
        match create_automation_task(ts, auto) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(
                    automation_id = %auto.id,
                    error = %e,
                    "automation trigger: failed to create task"
                );
                None
            }
        }
    } else {
        None
    };

    // 3. Emit AutomationTriggered event
    bus.emit(OrchestrationEvent::AutomationTriggered {
        automation_id: auto.id.clone(),
        automation_name: auto.name.clone(),
        task_id: task_id.clone().unwrap_or_default(),
    });

    // 4. Build agent overrides from automation config
    let session_id = format!("automation:{}", auto.id);
    let mut overrides = AgentOverrides::default();

    if let Some(ref model) = auto.model_id {
        if !model.is_empty() {
            overrides.model = Some(model.clone());
        }
    }

    if let Some(ref cwds) = auto.cwds {
        if !cwds.is_empty() {
            // Parse the JSON array of working directories and use the first one
            if let Ok(dirs) = serde_json::from_str::<Vec<String>>(cwds) {
                if let Some(first) = dirs.first() {
                    overrides.workspace = Some(first.clone());
                }
            } else {
                // Treat as a single path
                overrides.workspace = Some(cwds.clone());
            }
        }
    }

    // 5. Execute via unified dispatch path
    let result = agent.send_message(&session_id, &auto.prompt, overrides).await;

    // 6. Record run result
    let (status, error_text) = match &result {
        Ok(reply) => {
            let preview: String = reply.text.chars().take(200).collect();
            tracing::info!(
                automation_id = %auto.id,
                run_id = %run_id,
                preview = %preview,
                "automation run completed successfully"
            );
            ("success", None)
        }
        Err(e) => {
            tracing::warn!(
                automation_id = %auto.id,
                run_id = %run_id,
                error = %e,
                "automation run failed"
            );
            ("failed", Some(e.to_string()))
        }
    };

    let _ = svc.record_run_finish(
        &run_id,
        status,
        task_id.as_deref(),
        error_text.as_deref(),
    );

    // 7. Update runtime state (last_run_at, next_run_at, run_count)
    if let Err(e) = svc.update_after_run(&auto.id) {
        tracing::warn!(
            automation_id = %auto.id,
            error = %e,
            "automation trigger: failed to update runtime state"
        );
    }

    // 8. Optionally update Task status
    if let (Some(ts), Some(tid)) = (task_service, task_id) {
        let outcome = if result.is_ok() {
            ts.complete(&tid)
        } else {
            ts.fail(&tid)
        };
        if let Err(e) = outcome {
            tracing::warn!(
                task_id = %tid,
                error = %e,
                "automation trigger: failed to update task status"
            );
        }
    }
}

/// Create a Task for an automation run, linking it to the task tree.
fn create_automation_task(
    task_service: &hermix_task::TaskService,
    auto: &Automation,
) -> Result<String, String> {
    let workspace = auto
        .cwds
        .as_ref()
        .and_then(|cwds| {
            serde_json::from_str::<Vec<String>>(cwds)
                .ok()
                .and_then(|dirs| dirs.into_iter().next())
        })
        .unwrap_or_else(|| ".".to_string());

    let mut context = TaskContext::default();
    context.notes = Some(format!(
        "Automation: {}\nPrompt: {}",
        auto.name, auto.prompt
    ));

    let params = hermix_task::CreateTaskParams {
        title: format!("[Automation] {}", auto.name),
        workspace_path: workspace,
        mode: hermix_core::TaskMode::Craft,
        context,
        parent_task_id: None,
    };

    task_service
        .create(params)
        .map(|t| t.id)
        .map_err(|e| e.to_string())
}

/// Manually trigger an automation by ID. This is the server-side
/// implementation for `POST /v1/automations/{id}/trigger`.
/// It uses the same dispatch path as the scheduler — `agent_service.send_message()`.
pub async fn trigger_automation_manually(
    automation_id: &str,
    svc: &AutomationService,
    agent: &Arc<dyn AgentService>,
    bus: &OrchestrationEventBus,
    task_service: Option<&hermix_task::TaskService>,
) -> Result<String, String> {
    let auto = svc
        .get(automation_id)
        .map_err(|e| e.to_string())?;

    tracing::info!(
        automation_id = %auto.id,
        "manual automation trigger"
    );

    // Execute via the same path as scheduled triggers
    execute_automation_run(&auto, svc, agent, bus, task_service).await;

    Ok(auto.id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{AgentError, Message, StreamChunk};

    fn test_automation_service() -> (Arc<AutomationService>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(hermix_automation::AutomationRepository::new(
            tmp.path().join("test.db"),
        ));
        repo.ensure_schema().unwrap();
        (Arc::new(AutomationService::new(repo)), tmp)
    }

    /// Minimal noop agent service for testing.
    struct NoopAgent;

    #[async_trait::async_trait]
    impl AgentService for NoopAgent {
        async fn send_message(
            &self,
            _session_id: &str,
            _text: &str,
            _overrides: AgentOverrides,
        ) -> Result<hermix_core::traits::AgentReply, AgentError> {
            Ok(hermix_core::traits::AgentReply {
                text: "ok".into(),
                message_count: 1,
                finish_reason: Some("stop".into()),
                last_error: None,
                interrupted: false,
            })
        }
        async fn send_message_stream(
            &self,
            _session_id: &str,
            _text: &str,
            _overrides: AgentOverrides,
            _on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync>,
        ) -> Result<hermix_core::traits::AgentReply, AgentError> {
            Ok(hermix_core::traits::AgentReply {
                text: "ok".into(),
                message_count: 1,
                finish_reason: Some("stop".into()),
                last_error: None,
                interrupted: false,
            })
        }
        async fn get_session_messages(
            &self,
            _session_id: &str,
        ) -> Result<Vec<Message>, AgentError> {
            Ok(Vec::new())
        }
        async fn reset_session(&self, _session_id: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn test_create_automation_task() {
        let tmp = tempfile::tempdir().unwrap();
        let task_repo = Arc::new(hermix_task::TaskRepository::new(
            tmp.path().join("tasks.db"),
        ));
        task_repo.ensure_schema().unwrap();
        let task_svc = hermix_task::TaskService::new(task_repo);

        let auto = Automation::new(hermix_automation::CreateAutomationParams {
            name: "Test Auto".into(),
            prompt: "Do something".into(),
            schedule_type: hermix_automation::ScheduleType::Once,
            scheduled_at: Some("2026-01-01T00:00:00Z".into()),
            cwds: Some(serde_json::json!(["/tmp/workspace"]).to_string()),
            ..Default::default()
        });

        let task_id = create_automation_task(&task_svc, &auto).unwrap();
        assert!(!task_id.is_empty());

        let task = task_svc.get(&task_id).unwrap();
        assert!(task.title.contains("Test Auto"));
        assert_eq!(task.workspace_path, "/tmp/workspace");
    }

    #[tokio::test]
    async fn test_execute_automation_run_records_success() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Test".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        execute_automation_run(&auto, &svc, &agent, &bus, None).await;

        // Verify a run was recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn test_trigger_callback_fires_and_records() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Test".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        let trigger = build_trigger_callback(svc.clone(), agent, bus, None);
        trigger(auto.clone());

        // Give the spawned task time to complete
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify a run was recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty(), "a run should have been recorded");
        assert_eq!(runs[0].status, "success");
    }

    #[tokio::test]
    async fn test_manual_trigger() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Manual Test".into(),
                prompt: "Run manually".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        let result = trigger_automation_manually(&auto.id, &svc, &agent, &bus, None).await;
        assert!(result.is_ok());

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty());
        assert_eq!(runs[0].status, "success");
    }

    #[tokio::test]
    async fn test_scheduler_handle_shutdown() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let handle = start_automation_scheduler(svc, agent, bus, None);

        // Shutdown should not panic
        handle.shutdown();
    }

    #[tokio::test]
    async fn test_execute_with_task_service_creates_task() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let tmp = tempfile::tempdir().unwrap();
        let task_repo = Arc::new(hermix_task::TaskRepository::new(
            tmp.path().join("tasks.db"),
        ));
        task_repo.ensure_schema().unwrap();
        let task_svc = hermix_task::TaskService::new(task_repo);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Task Test".into(),
                prompt: "Do work".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        execute_automation_run(&auto, &svc, &agent, &bus, Some(&task_svc)).await;

        // Verify a task was created and completed
        let tasks = task_svc.list(None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].title.contains("Task Test"));
        assert_eq!(tasks[0].status, hermix_core::TaskStatus::Completed);

        // Verify the run was also recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].task_id.is_some());
    }
}
