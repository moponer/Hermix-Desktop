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
//! 1. `AutomationScheduler` ticks → finds due automation → **claims the
//!    run synchronously** (`AutomationService::try_begin_run`)
//! 2. `TriggerFn` receives the claimed run id and spawns a tokio task
//! 3. The task marks the run dispatched, then calls
//!    `agent_service.send_message()`
//! 4. On completion: records the run result + updates runtime state
//! 5. Emits `AutomationTriggered` event via `OrchestrationEventBus`
//! 6. Optionally creates a Task (if `task_service` is available) and
//!    links it to the automation run
//!
//! ## Why the claim is synchronous (and not inside the spawned task)
//!
//! It used to be the first statement of `execute_automation_run`, i.e.
//! inside the `tokio::spawn`. `trigger()` then returned with no run row
//! written and `last_run_at` still stale, so if the spawned task was not
//! scheduled promptly the next 30s tick judged the automation due again
//! and fired it a second time. Claiming inline closes that window.

use std::sync::Arc;
use std::time::Duration;

use hermix_automation::{
    classify_dispatch_failure, decide_delivery_target, dispatch_outcome, Automation,
    AutomationScheduler, AutomationService, DeliveryGuardInput, DeliveryTarget, DispatchOutcome,
    FailureClass, RunStatus, RunTrigger, TriggerFn,
};
use hermix_core::traits::{AgentOverrides, AgentService, ExternalDelivery};
use hermix_core::TaskContext;

use crate::orchestration_bus::{OrchestrationEvent, OrchestrationEventBus};

/// Answers "does this conversation still exist?".
///
/// A trait rather than a direct call into the session store so this
/// module keeps no dependency on the persistence crate's internals, and
/// so the guardrail can be tested without a real database. Production
/// wires it to `SessionPersistence::get_session_title`.
pub trait SessionProbe: Send + Sync {
    fn session_exists(&self, session_id: &str) -> bool;
}

/// Resolve where a run's result should be delivered, gathering the
/// guardrail's inputs from the run table plus one existence check.
fn decide_delivery(
    auto: &Automation,
    run_id: &str,
    svc: &AutomationService,
    probe: Option<&dyn SessionProbe>,
) -> DeliveryTarget {
    let fallback_session_id = format!("automation:{}:{}", auto.id, run_id);

    // Without a probe (tests, or a host that does not expose the session
    // store) the anchor is taken on faith. That matches the behaviour
    // from before the guardrail existed, and is the safe direction: it
    // only means the existence check is skipped, not inverted.
    let anchor_exists = match auto.anchor_session_id.as_deref().filter(|s| !s.is_empty()) {
        Some(anchor) => probe.map(|p| p.session_exists(anchor)).unwrap_or(true),
        None => false,
    };

    let prior_injections = auto
        .anchor_session_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|anchor| svc.count_deliveries_into(&auto.id, anchor).ok())
        .unwrap_or(0);
    let consecutive_failures = svc
        .consecutive_terminal_failures(&auto.id)
        .unwrap_or(0)
        .max(0) as u64;

    decide_delivery_target(&DeliveryGuardInput {
        anchor_session_id: auto.anchor_session_id.clone(),
        prior_injections,
        consecutive_failures,
        anchor_exists,
        fallback_session_id,
        max_injections: None,
        max_consecutive_failures: None,
    })
}

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
    session_probe: Option<Arc<dyn SessionProbe>>,
) -> AutomationSchedulerHandle {
    let trigger = build_trigger_callback(
        automation_service.clone(),
        agent_service,
        orchestration_bus,
        task_service,
        session_probe,
    );

    let scheduler = AutomationScheduler::start(
        automation_service,
        trigger,
        Duration::from_secs(30),
    );

    AutomationSchedulerHandle { scheduler }
}

/// Build the trigger callback that fires when an automation is due.
///
/// The run has already been claimed synchronously by the scheduler, so
/// this callback only has to spawn the work and hand the run id down.
fn build_trigger_callback(
    automation_service: Arc<AutomationService>,
    agent_service: Arc<dyn AgentService>,
    orchestration_bus: OrchestrationEventBus,
    task_service: Option<Arc<hermix_task::TaskService>>,
    session_probe: Option<Arc<dyn SessionProbe>>,
) -> TriggerFn {
    Arc::new(move |auto: Automation, run_id: String| {
        let svc = automation_service.clone();
        let agent = agent_service.clone();
        let bus = orchestration_bus.clone();
        let tasks = task_service.clone();
        let probe = session_probe.clone();

        tokio::spawn(async move {
            execute_automation_run(
                &auto,
                &run_id,
                &svc,
                &agent,
                &bus,
                tasks.as_deref(),
                probe.as_deref(),
            )
            .await;
        });
    })
}

/// Execute a single automation run end-to-end.
/// This is the core of the unified dispatch path: it calls
/// `agent_service.send_message()`, which internally runs the full
/// `AgentLoop` with `ToolRegistry::dispatch_with_context()` — the same
/// path as HTTP chat requests.
async fn execute_automation_run(
    auto: &Automation,
    run_id: &str,
    svc: &AutomationService,
    agent: &Arc<dyn AgentService>,
    bus: &OrchestrationEventBus,
    task_service: Option<&hermix_task::TaskService>,
    session_probe: Option<&dyn SessionProbe>,
) {
    tracing::info!(
        automation_id = %auto.id,
        automation_name = %auto.name,
        run_id = %run_id,
        "automation triggered — dispatching via agent_service.send_message()"
    );

    // 1. Mark dispatched. A run that never reaches this point is reaped to
    //    `abandoned` at next startup, which is exactly the signal that the
    //    process died between claim and hand-off.
    if let Err(e) = svc.mark_dispatched(run_id) {
        tracing::warn!(
            automation_id = %auto.id,
            run_id = %run_id,
            error = %e,
            "automation trigger: failed to mark run dispatched"
        );
    }

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
    //
    //    Delivery is decided before dispatch, not after: the guardrail
    //    reads the anchor's injection count and the run's failure streak,
    //    and `send_message` UPSERTs the sessions row — dispatching into a
    //    deleted anchor would resurrect it as a title-less orphan rather
    //    than falling back.
    let target = decide_delivery(&auto, run_id, svc, session_probe.as_deref());

    if !target.is_anchored() {
        tracing::info!(
            automation_id = %auto.id,
            run_id = %run_id,
            "automation result falling back to a standalone session"
        );
    }

    let session_id = target.session_id().to_string();
    let mut overrides = AgentOverrides::default();

    // Marks this turn as externally triggered, which is what makes
    // `LocalAgentService` store the prompt and the reply under
    // `MessageRole::Automation` instead of `user` / `assistant`. Without
    // it the run leaves a fabricated "the user asked this" line in the
    // conversation and the model, seeing it next turn, tries to carry on
    // answering a question nobody asked.
    overrides.external_delivery = Some(ExternalDelivery {
        source_label: auto.name.clone(),
        run_id: Some(run_id.to_string()),
        fallback_reason: match &target {
            DeliveryTarget::Anchor(_) => None,
            DeliveryTarget::Standalone { reason, .. } => Some(reason.clone()),
        },
    });

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

    // 6. Record run result.
    //
    //    Two things are being decided here, and they are not the same
    //    question. *Did it work* comes from the reply: `AgentLoop::run()`
    //    turns an LLM/transport failure into `Ok(..)` with
    //    `finish_reason = "error"` so the transcript survives, so an
    //    `Err` here is nearly always local misconfiguration. *May it be
    //    retried* comes from `dispatch_outcome`, which needs the turn
    //    count — a run that already executed tools must never be replayed
    //    no matter how transient the error looks.
    let outcome = match &result {
        Ok(reply) => dispatch_outcome(reply),
        Err(e) => match classify_dispatch_failure(e) {
            FailureClass::Transient => DispatchOutcome::DispatchFailed { retryable: true },
            FailureClass::Permanent => DispatchOutcome::Failed { retryable: false },
        },
    };

    let status = match outcome {
        DispatchOutcome::Succeeded => RunStatus::Succeeded,
        DispatchOutcome::Failed { .. } => RunStatus::Failed,
        DispatchOutcome::DispatchFailed { .. } => RunStatus::FailedToDispatch,
    };
    let error_text = match &result {
        Ok(reply) => reply.last_error.clone(),
        Err(e) => Some(e.to_string()),
    };

    match &result {
        Ok(reply) if status == RunStatus::Succeeded => {
            let preview: String = reply.text.chars().take(200).collect();
            tracing::info!(
                automation_id = %auto.id,
                run_id = %run_id,
                preview = %preview,
                "automation run completed successfully"
            );
        }
        Ok(reply) => tracing::warn!(
            automation_id = %auto.id,
            run_id = %run_id,
            total_turns = reply.total_turns,
            error = ?reply.last_error,
            status = %status.as_str(),
            "automation run failed"
        ),
        Err(e) => tracing::warn!(
            automation_id = %auto.id,
            run_id = %run_id,
            error = %e,
            "automation dispatch failed"
        ),
    }

    let _ = svc.record_run_finish(
        run_id,
        status,
        task_id.as_deref(),
        error_text.as_deref(),
    );

    // 6a. Book the delivery against the guardrail's counter.
    //
    //     Only anchored runs are recorded: a run that fell back to its
    //     own session never touched the user's conversation, so counting
    //     it would spend an allowance it did not use. Recorded after the
    //     run finished, because a failed run is not an injection.
    if target.is_anchored() {
        if let Err(e) = svc.mark_run_delivered(run_id, &session_id) {
            tracing::warn!(
                automation_id = %auto.id,
                run_id = %run_id,
                error = %e,
                "automation: failed to record delivery target"
            );
        }
    }

    // 6b. Retry only what provably never reached a tool.
    //
    //     The retry row is written *after* the terminal one so the history
    //     shows the attempt that failed; `defer_for_retry` then pins the
    //     schedule to the retry instant. `run_count` is left alone
    //     because nothing executed.
    let retry_at = match outcome {
        DispatchOutcome::DispatchFailed { retryable: true }
        | DispatchOutcome::Failed { retryable: true } => {
            match svc.schedule_dispatch_retry(
                run_id,
                &auto.id,
                error_text.as_deref().unwrap_or("dispatch failed"),
            ) {
                Ok(Some(at)) => Some(at),
                Ok(None) => {
                    tracing::warn!(
                        automation_id = %auto.id,
                        run_id = %run_id,
                        "automation: giving up after the dispatch attempt cap"
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "automation: failed to schedule retry");
                    None
                }
            }
        }
        _ => None,
    };
    if let Some(at) = retry_at {
        if let Err(e) = svc.defer_for_retry(&auto.id, at) {
            tracing::warn!(error = %e, "automation: failed to defer for retry");
        }
    }

    // 7. Update runtime state (last_run_at, next_run_at, run_count).
    //    Skipped when a retry was scheduled: `update_after_run` recomputes
    //    `next_run_at` from the cron expression and would overwrite the
    //    backoff we just set, and it moves `last_run_at` forward, which
    //    `is_due` reads as "this slot already ran" — the retry would
    //    never fire. A deferred run is not a completed run.
    if retry_at.is_none() {
        if let Err(e) = svc.update_after_run(&auto.id) {
            tracing::warn!(
                automation_id = %auto.id,
                error = %e,
                "automation trigger: failed to update runtime state"
            );
        }
    }

    // 8. Optionally update Task status. Keyed on the run status, not on
    //    `result.is_ok()`: a provider outage comes back as `Ok` with
    //    `finish_reason = "error"`, and that task did not get done.
    if let (Some(ts), Some(tid)) = (task_service, task_id) {
        let outcome = if status == RunStatus::Succeeded {
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
        // The automation's task belongs to the session it will be
        // delivered to (the anchor), so it shows up in that session's
        // task list instead of floating unclaimed.
        session_id: auto.anchor_session_id.clone(),
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
    session_probe: Option<&dyn SessionProbe>,
) -> Result<String, String> {
    let auto = svc
        .get(automation_id)
        .map_err(|e| e.to_string())?;

    // Claim through the same door as the scheduler. Without this, a user
    // clicking "run now" while a scheduled run is in flight would start a
    // second agent run writing to the same automation.
    let run_id = svc
        .try_begin_run(&auto.id, RunTrigger::Manual, None)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!("自动化「{}」已有任务在执行中，请等它结束", auto.name)
        })?;

    tracing::info!(
        automation_id = %auto.id,
        run_id = %run_id,
        "manual automation trigger"
    );

    // Execute via the same path as scheduled triggers
    execute_automation_run(&auto, &run_id, svc, agent, bus, task_service, session_probe).await;

    Ok(auto.id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{AgentError, Message, StreamChunk};
    use std::sync::Mutex;

    fn test_automation_service() -> (Arc<AutomationService>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(hermix_automation::AutomationRepository::new(
            tmp.path().join("test.db"),
        ));
        repo.ensure_schema().unwrap();
        (Arc::new(AutomationService::new(repo)), tmp)
    }

    /// 复现调度器的同步 claim 顺序，返回 run id。
    /// 直接调 `execute_automation_run` 的测试必须先 claim——它现在假定
    /// run 已存在（claim 是调度器的职责，不是它的）。
    fn claim(svc: &AutomationService, automation_id: &str) -> String {
        svc.try_begin_run(automation_id, RunTrigger::Manual, None)
            .unwrap()
            .expect("claim should succeed on a fresh automation")
    }

    /// 一个可编程的 agent：记录每次收到的 session id，并按预设脚本回放
    /// reply。默认回"正常完成"，`reply` 字段可换成错误 reply 来测失败路径。
    ///
    /// `AgentLoop::run()` 把 LLM 失败表达成 `Ok` + `finish_reason="error"`，
    /// 所以"失败"必须靠 reply 的内容表达，靠 `Err` 表达不了真实故障。
    struct ScriptedAgent {
        sessions: Arc<Mutex<Vec<String>>>,
        /// 逐次调用返回的 reply；用尽后重复最后一个。
        replies: std::sync::Mutex<Vec<hermix_core::traits::AgentReply>>,
        /// 每次调用收到的 `AgentOverrides`，顺序与 `sessions` 对齐。
        /// `external_delivery` 的接线只能在这里验——它是"这轮不是人
        /// 在打字"的唯一标记，且不落到任何 DB 列上。
        overrides: Arc<Mutex<Vec<AgentOverrides>>>,
    }

    impl ScriptedAgent {
        fn always_ok(sessions: Arc<Mutex<Vec<String>>>) -> Self {
            Self::scripted(sessions, vec![reply_ok()])
        }

        fn scripted(
            sessions: Arc<Mutex<Vec<String>>>,
            replies: Vec<hermix_core::traits::AgentReply>,
        ) -> Self {
            Self {
                sessions,
                replies: std::sync::Mutex::new(replies),
                overrides: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    /// 正常完成的 reply。
    fn reply_ok() -> hermix_core::traits::AgentReply {
        hermix_core::traits::AgentReply {
            text: "ok".into(),
            message_count: 1,
            finish_reason: Some("stop".into()),
            last_error: None,
            interrupted: false,
            total_turns: 1,
        }
    }

    /// `AgentLoop` 在 LLM 失败时构造的那种 reply。
    fn reply_errored(total_turns: u32, last_error: &str) -> hermix_core::traits::AgentReply {
        hermix_core::traits::AgentReply {
            text: "✅ 已执行完毕（本轮未生成可见文本总结）。".into(),
            message_count: 1,
            finish_reason: Some("error".into()),
            last_error: Some(last_error.into()),
            interrupted: true,
            total_turns,
        }
    }

    #[async_trait::async_trait]
    impl AgentService for ScriptedAgent {
        async fn send_message(
            &self,
            session_id: &str,
            _text: &str,
            overrides: AgentOverrides,
        ) -> Result<hermix_core::traits::AgentReply, AgentError> {
            self.sessions.lock().unwrap().push(session_id.to_string());
            self.overrides.lock().unwrap().push(overrides);
            let mut q = self.replies.lock().unwrap();
            if q.len() > 1 {
                Ok(q.remove(0))
            } else {
                Ok(q[0].clone())
            }
        }
        async fn send_message_stream(
            &self,
            _session_id: &str,
            _text: &str,
            _overrides: AgentOverrides,
            _on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync>,
        ) -> Result<hermix_core::traits::AgentReply, AgentError> {
            unreachable!("automation uses the non-streaming path")
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
                total_turns: 1,
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
                total_turns: 1,
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

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, None).await;

        // Verify a run was recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert!(runs[0].finished_at.is_some());
        // claim → dispatched 的状态迁移必须留下痕迹。
        assert!(
            runs[0].dispatched_at.is_some(),
            "run 必须先标记 dispatched 才交给 agent"
        );
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

        let trigger = build_trigger_callback(svc.clone(), agent, bus, None, None);
        // 调度器在调用 trigger 之前就已同步 claim；这里复现同一顺序。
        let run_id = claim(&svc, &auto.id);
        trigger(auto.clone(), run_id);

        // Give the spawned task time to complete
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify a run was recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty(), "a run should have been recorded");
        assert_eq!(runs[0].status, RunStatus::Succeeded);
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

        let result = trigger_automation_manually(&auto.id, &svc, &agent, &bus, None, None).await;
        assert!(result.is_ok());

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty());
        assert_eq!(runs[0].status, RunStatus::Succeeded);
    }

    /// 手动触发必须走同一道 claim 门：在途 run 存在时要明确拒绝，
    /// 而不是起第二个 agent 往同一个自动化里写。
    #[tokio::test]
    async fn test_manual_trigger_rejected_while_run_in_flight() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Busy".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        // 手工 claim 一个在途 run，不收尾。
        svc.try_begin_run(&auto.id, RunTrigger::Manual, None)
            .unwrap()
            .unwrap();

        let err = trigger_automation_manually(&auto.id, &svc, &agent, &bus, None, None)
            .await
            .expect_err("在途 run 期间手动触发必须被拒");
        assert!(
            err.contains("执行中"),
            "错误文案要说清是「已有任务在执行」，实际: {err}"
        );
    }

    /// 启动收割后，遗留的在途行不再阻塞手动触发。
    #[tokio::test]
    async fn test_reap_unblocks_manual_trigger() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Recovered".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();

        svc.try_begin_run(&auto.id, RunTrigger::Manual, None)
            .unwrap()
            .unwrap();
        // 模拟"进程重启"：收割把在途行关掉。
        assert_eq!(svc.reap_abandoned_runs().unwrap(), 1);
        assert_eq!(
            svc.count_active_runs().unwrap(),
            0,
            "收割后不得还有在途行，否则保活信号会把机器永久按在唤醒状态"
        );

        let result = trigger_automation_manually(&auto.id, &svc, &agent, &bus, None, None).await;
        assert!(result.is_ok(), "收割后应能正常触发: {result:?}");
    }

    /// 无锚点的自动化走 `automation:{id}:{run_id}` 回退会话，两次 run
    /// 不同会话——否则多次运行会共用一段不断增长的历史。
    #[tokio::test]
    async fn test_unanchored_session_id_includes_run_id() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let sessions = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent: Arc<dyn AgentService> = Arc::new(ScriptedAgent::always_ok(sessions.clone()));
        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Unanchored".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(auto.anchor_session_id.is_none());

        for slot in [1i64, 2] {
            let run_id = svc
                .try_begin_run(&auto.id, RunTrigger::Scheduled, Some(slot * 1000))
                .unwrap()
                .unwrap();
            execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, None).await;
        }

        let seen = sessions.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1], "两次 run 不得共用同一会话");
        assert!(seen[0].starts_with(&format!("automation:{}:", auto.id)));
    }

    /// 首轮 provider 故障 → 记 `failed_to_dispatch` 并安排退避重试。
    ///
    /// 这是退避机制在生产路径上的唯一入口。若这条断言不成立，
    /// `MAX_DISPATCH_ATTEMPTS` / `backoff_delay_ms` / `defer_for_retry`
    /// 全都是死代码，而 provider 挂掉会静默地什么都不做。
    #[tokio::test]
    async fn test_transient_first_turn_failure_schedules_retry() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let sessions = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent: Arc<dyn AgentService> = Arc::new(ScriptedAgent::scripted(
            sessions,
            vec![reply_errored(1, "API error 503: service unavailable")],
        ));

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Flaky".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, None).await;

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(
            runs[0].status,
            RunStatus::FailedToDispatch,
            "首轮失败且零工具执行，必须记为派发失败而非执行失败"
        );

        // 退避被写进 runtime state：next_run_at 前移、last_run_at 不动、
        // run_count 不增（派发没到 agent，不算一次执行）。
        let state = svc.get_runtime_state(&auto.id).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let next = state.next_run_at.expect("退避必须安排 next_run_at");
        assert!(
            next > now,
            "退避时刻应在未来，实际 {next} vs now {now}"
        );
        assert_eq!(
            state.run_count, 0,
            "派发未达 agent，不该计入执行次数"
        );
        assert!(
            state.last_run_at.is_none(),
            "update_after_run 被退避分支跳过时 last_run_at 应保持原值"
        );
    }

    /// 工具跑过之后失败 → 记 `failed`，**不安排重试**。
    ///
    /// 与上一条构成对照：两次的差别只有 `total_turns`。重放会重复已
    /// 发生的副作用，代价远大于漏掉一次重试。
    #[tokio::test]
    async fn test_failure_after_tools_is_not_retried() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let sessions = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent: Arc<dyn AgentService> = Arc::new(ScriptedAgent::scripted(
            sessions,
            vec![reply_errored(4, "API error 503: service unavailable")],
        ));

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "SideEffects".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, None).await;

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(
            runs[0].status,
            RunStatus::Failed,
            "跑过工具的 run 必须记执行失败"
        );
        let state = svc.get_runtime_state(&auto.id).unwrap();
        assert_eq!(
            state.run_count, 1,
            "不重试的失败按正常收尾记账"
        );
    }

    /// provider 挂掉时不能记成"成功"——这是本次修复的核心漏洞。
    ///
    /// 回归防护：`AgentLoop::run()` 把 LLM 失败表达成 `Ok` + 
    /// `finish_reason = "error"`，任何按 `result.is_ok()` 记账的地方都会
    /// 把三小时的 provider 停机写成一片绿。
    #[tokio::test]
    async fn test_errored_reply_is_not_recorded_as_success() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let sessions = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent: Arc<dyn AgentService> = Arc::new(ScriptedAgent::scripted(
            sessions,
            vec![reply_errored(1, "API error 503: service unavailable")],
        ));

        let task_tmp = tempfile::tempdir().unwrap();
        let task_repo = Arc::new(hermix_task::TaskRepository::new(
            task_tmp.path().join("tasks.db"),
        ));
        task_repo.ensure_schema().unwrap();
        let task_svc = hermix_task::TaskService::new(task_repo);

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Outage".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Recurring,
                rrule: Some("0 8 * * *".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, Some(&task_svc), None).await;

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_ne!(
            runs[0].status,
            RunStatus::Succeeded,
            "finish_reason=error 的 reply 绝不能记成成功"
        );
        assert!(
            runs[0].error_text.as_deref().unwrap_or_default().contains("503"),
            "原始错误文本要留在 run 记录里，实际: {:?}",
            runs[0].error_text
        );
        let tasks = task_svc.list(None, None).unwrap();
        assert_eq!(
            tasks[0].status,
            hermix_core::TaskStatus::Failed,
            "Task 也必须记失败，不能按 result.is_ok() 记成完成"
        );
    }

    /// 有锚点的自动化直接用来源对话，不走回退路径。
    #[tokio::test]
    async fn test_anchored_automation_uses_anchor_session() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let agent: Arc<dyn AgentService> = Arc::new(ScriptedAgent::always_ok(seen.clone()));

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "Anchored".into(),
                prompt: "Hello".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                anchor_session_id: Some("sess-xyz".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(auto.anchor_session_id.as_deref(), Some("sess-xyz"));

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, None).await;

        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["sess-xyz".to_string()],
            "锚定自动化必须用来源对话，不能落到 automation:* 回退路径"
        );
    }

    /// 锚点存在时：投锚点 + `external_delivery` 正确设置 + 回填
    /// `delivered_to_session_id`。
    ///
    /// 这三件事串起来才是"结果出现在用户对话里"的完整闭环：
    /// session 选错 → 结果去了别处；overrides 漏设 → 落库成 user/
    /// assistant 假轮次；回填漏掉 → 注入护栏按 0 计数，永不触发。
    #[tokio::test]
    async fn test_anchored_run_sets_external_delivery_and_books_delivery() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        // 持具体类型以便读回记录的 overrides；`Arc<ScriptedAgent>` 向上
        // 转型成 `Arc<dyn AgentService>` 指向同一实例。
        let scripted = Arc::new(ScriptedAgent::always_ok(seen.clone()));
        let agent: Arc<dyn AgentService> = scripted.clone();

        struct Exists;
        impl SessionProbe for Exists {
            fn session_exists(&self, _session_id: &str) -> bool {
                true
            }
        }

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "每日巡检".into(),
                prompt: "检查磁盘".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                anchor_session_id: Some("anchor-1".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, Some(&Exists)).await;

        assert_eq!(seen.lock().unwrap().as_slice(), ["anchor-1".to_string()]);

        let ov = scripted.overrides.lock().unwrap();
        let delivery = ov[0]
            .external_delivery
            .as_ref()
            .expect("外部投递轮次必须带 external_delivery，否则落库成假 user/assistant 轮次");
        assert_eq!(delivery.source_label, "每日巡检");
        assert_eq!(delivery.run_id.as_deref(), Some(run_id.as_str()));
        assert!(
            delivery.fallback_reason.is_none(),
            "投进锚点不是回退，不能带原因"
        );

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(
            runs[0].delivered_to_session_id.as_deref(),
            Some("anchor-1"),
            "锚定投递必须回填——不回填护栏的注入计数恒为 0，永不触发"
        );
        assert_eq!(svc.count_deliveries_into(&auto.id, "anchor-1").unwrap(), 1);
    }

    /// 锚点已删时：投独立会话 + 带 fallback_reason + **不**回填计数。
    ///
    /// 反向验证上一条：回退路径三件事必须同时成立。特别是不回填——
    /// 结果根本没进那个会话，记了它会让护栏白吃额度。
    #[tokio::test]
    async fn test_fallback_run_targets_standalone_and_explains_why() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let scripted = Arc::new(ScriptedAgent::always_ok(seen.clone()));
        let agent: Arc<dyn AgentService> = scripted.clone();

        struct Gone;
        impl SessionProbe for Gone {
            fn session_exists(&self, _session_id: &str) -> bool {
                false
            }
        }

        let auto = svc
            .create(hermix_automation::CreateAutomationParams {
                name: "每日巡检".into(),
                prompt: "检查磁盘".into(),
                schedule_type: hermix_automation::ScheduleType::Once,
                scheduled_at: Some("2026-01-01T00:00:00Z".into()),
                anchor_session_id: Some("deleted-anchor".into()),
                ..Default::default()
            })
            .unwrap();

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, None, Some(&Gone)).await;

        let target = format!("automation:{}:{}", auto.id, run_id);
        assert_eq!(seen.lock().unwrap().as_slice(), [target.clone()]);

        let ov = scripted.overrides.lock().unwrap();
        let delivery = ov[0].external_delivery.as_ref().unwrap();
        assert_eq!(delivery.source_label, "每日巡检");
        let reason = delivery
            .fallback_reason
            .as_deref()
            .expect("回退必须带原因——没有解释的回退在用户眼里等于结果消失");
        assert!(
            !reason.is_empty(),
            "回退原因不能是空串，那是把问题从后端搬到前端"
        );

        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert_eq!(
            runs[0].delivered_to_session_id, None,
            "回退没碰用户对话，不能记注入计数"
        );
        assert_eq!(svc.count_deliveries_into(&auto.id, "deleted-anchor").unwrap(), 0);
    }

    #[tokio::test]
    async fn test_scheduler_handle_shutdown() {
        let (svc, _tmp) = test_automation_service();
        let bus = OrchestrationEventBus::new();
        let agent: Arc<dyn AgentService> = Arc::new(NoopAgent);

        let handle = start_automation_scheduler(svc, agent, bus, None, None);

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

        let run_id = claim(&svc, &auto.id);
        execute_automation_run(&auto, &run_id, &svc, &agent, &bus, Some(&task_svc), None).await;

        // Verify a task was created and completed
        let tasks = task_svc.list(None, None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].title.contains("Task Test"));
        assert_eq!(tasks[0].status, hermix_core::TaskStatus::Completed);

        // Verify the run was also recorded
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].task_id.is_some());
    }

}
