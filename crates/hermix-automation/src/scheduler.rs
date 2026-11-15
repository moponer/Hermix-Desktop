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

use crate::{Automation, AutomationService, AutomationStatus, RunTrigger, ScheduleType};

/// Callback invoked when an automation is due to fire.
///
/// The callback receives the automation and the run id it was claimed
/// under, and is responsible for spawning the agent execution. The run
/// row is written **before** this is called (see
/// [`AutomationService::try_begin_run`]) — that ordering is what stops a
/// slow spawn from letting the next tick double-fire.
pub type TriggerFn = Arc<dyn Fn(Automation, String) + Send + Sync>;

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
    tick_once_at(service, trigger, chrono::Utc::now().timestamp_millis()).await
}

/// The tick body with an injected clock.
///
/// `now` is a parameter rather than a call to `Utc::now()` inside the loop
/// because the only interesting question this function answers — "is the
/// deferred retry due *yet*?" — is a comparison against a wall clock. A test
/// that has to set up that comparison with real sleeps is measuring the
/// disk: every repository call opens its own connection, and on a slow
/// drive the few hundred milliseconds of setup silently consume the backoff
/// window, so the test passes or fails for reasons that have nothing to do
/// with the code under test.
async fn tick_once_at(
    service: &AutomationService,
    trigger: &TriggerFn,
    now: i64,
) -> Result<(), String> {
    let automations = service
        .list(Some(AutomationStatus::Active))
        .map_err(|e| e.to_string())?;

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

        // Check if this automation should fire now.
        let should_fire = is_due(auto.schedule_type, state.next_run_at, state.last_run_at, now);

        if should_fire {
            // Claim synchronously, before the spawn. The `scheduled_at`
            // we pass is the slot that was *due* (`state.next_run_at`),
            // not the wall clock — two ticks inside one interval must
            // resolve to the same run id and dedupe, whereas a fresh
            // timestamp each tick would defeat the whole contract.
            let due_at = state.next_run_at;
            match service.try_begin_run(&auto.id, RunTrigger::Scheduled, due_at) {
                Ok(Some(run_id)) => {
                    tracing::info!(
                        automation_id = %auto.id,
                        automation_name = %auto.name,
                        run_id = %run_id,
                        "automation tick: firing automation"
                    );
                    trigger(auto.clone(), run_id);
                }
                // A declined claim is not an error: another run holds the
                // slot, the schedule ended, or this slot already ran.
                Ok(None) => {
                    tracing::debug!(
                        automation_id = %auto.id,
                        "automation tick: claim declined, not firing"
                    );
                }
                Err(e) => {
                    // Log and continue — one automation's storage trouble
                    // must not stop the whole tick.
                    tracing::warn!(
                        automation_id = %auto.id,
                        error = %e,
                        "automation tick: claim failed, skipping"
                    );
                }
            }
        }
    }

    Ok(())
}

/// The "is this automation due" predicate, as a pure function of four values.
///
/// Extracted from the tick loop so the retry contract can be pinned without
/// a database. The `last_run_at < next_run_at` comparison is load-bearing
/// for retry: a deferred dispatch moves `next_run_at` forward and
/// deliberately leaves `last_run_at` alone
/// (`AutomationService::defer_for_retry`), which keeps the condition true
/// so the retry actually fires. Touching `last_run_at` in the deferral
/// would silently disable retries; clearing `next_run_at` instead would
/// spin the tick at full speed. If you change this predicate, re-read
/// `defer_for_retry` and the retry tests below.
fn is_due(
    schedule_type: ScheduleType,
    next_run_at: Option<i64>,
    last_run_at: Option<i64>,
    now: i64,
) -> bool {
    match schedule_type {
        ScheduleType::Recurring => {
            // Fire if next_run_at is set and <= now, and we haven't
            // already fired since the last run
            match next_run_at {
                Some(next) if next <= now => match last_run_at {
                    Some(last) => last < next,
                    None => true,
                },
                _ => false,
            }
        }
        ScheduleType::Once => {
            // Fire if scheduled_at is set and <= now, and we haven't fired yet
            match next_run_at {
                Some(next) if next <= now => last_run_at.is_none(),
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutomationRepository, CreateAutomationParams, RunStatus};
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
        let trigger: TriggerFn = Arc::new(move |a, _run_id| {
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
        let trigger: TriggerFn = Arc::new(move |a, _run_id| {
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
        let trigger: TriggerFn = Arc::new(move |a, _run_id| {
            fired_clone.lock().unwrap().push(a.id);
        });

        tick_once(&svc, &trigger).await.unwrap();

        assert_eq!(fired.lock().unwrap().len(), 0);
    }

    /// 记录每次 tick 收到的 run_id，用来验证 claim 契约。
    fn recording_trigger() -> (TriggerFn, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let trigger: TriggerFn = Arc::new(move |_a, run_id| {
            seen_clone.lock().unwrap().push(run_id);
        });
        (trigger, seen)
    }

    /// 两个 tick 落在同一个 due slot 上时，只有第一次该触发。
    ///
    /// 这是 runId 契约存在的理由：tick 循环 30s 一跳，而自动化可能
    /// 每分钟一次——若 claim 不去重，同一分钟内会跑两次。
    #[tokio::test]
    async fn test_same_slot_fires_once_across_ticks() {
        let (svc, _tmp) = service();
        // 每分钟触发，且把 due 时间设成过去，保证两个 tick 都判为 due。
        let auto = svc
            .create(CreateAutomationParams {
                name: "PerMinute".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("* * * * *".into()),
                ..Default::default()
            })
            .unwrap();

        // 把 next_run_at 拉到过去，last_run_at 清空 → should_fire 为真。
        let base = chrono::Utc::now().timestamp_millis();
        svc.defer_for_retry(&auto.id, base - 60_000).unwrap();

        let (trigger, seen) = recording_trigger();
        tick_once_at(&svc, &trigger, base).await.unwrap();
        tick_once_at(&svc, &trigger, base).await.unwrap();
        tick_once_at(&svc, &trigger, base).await.unwrap();

        let fired = seen.lock().unwrap();
        assert_eq!(
            fired.len(),
            1,
            "同一 due slot 只能触发一次，实际触发了 {} 次：{:?}",
            fired.len(),
            fired
        );
    }

    /// 已 claim 但未完成的 run 挡住下一次触发（单机单飞）。
    #[tokio::test]
    async fn test_in_flight_run_blocks_next_tick() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "PerMinute".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("* * * * *".into()),
                ..Default::default()
            })
            .unwrap();
        let base = chrono::Utc::now().timestamp_millis();
        svc.defer_for_retry(&auto.id, base - 60_000).unwrap();

        // 手工 claim 一个在途 run（不 finish）。
        svc.try_begin_run(&auto.id, RunTrigger::Manual, None).unwrap();

        let (trigger, seen) = recording_trigger();
        tick_once_at(&svc, &trigger, base).await.unwrap();

        assert_eq!(
            seen.lock().unwrap().len(),
            0,
            "已有在途 run 时，tick 不得再触发"
        );
        // 且这次拒绝要留下 skipped 记录，UI 才知道"到点但没跑"。
        let runs = svc.list_runs(&auto.id, 10).unwrap();
        assert!(runs.iter().any(|r| r.status == RunStatus::Skipped));
    }

    /// 退避到期后重试仍能触发——方案标注的"最脆弱一处"。
    ///
    /// `should_fire` 要求 `next_run_at <= now && last_run_at < next_run_at`。
    /// `defer_for_retry` 把 `next_run_at` 推到退避时刻、且**不动**
    /// `last_run_at`——两个条件才同时成立。若它顺带改了 `last_run_at`
    /// （哪怕设成更早），重试就永不触发。
    ///
    /// 时刻全部由 `tick_once_at` 注入，不 sleep：把退避窗口设成"未来几十
    /// 毫秒再等过去"测的是机器快慢，不是退避是否生效——每次仓储调用都要
    /// `open_with_wal`，几十毫秒的窗口在慢盘上会被写操作整个吃掉。
    #[tokio::test]
    async fn test_deferred_retry_still_fires_after_backoff() {
        let (svc, _tmp) = service();
        let auto = svc
            .create(CreateAutomationParams {
                name: "Retrying".into(),
                prompt: "Test".into(),
                schedule_type: ScheduleType::Recurring,
                rrule: Some("* * * * *".into()),
                ..Default::default()
            })
            .unwrap();

        // 基准时刻。`update_after_run` 里的 `last_run_at` 取的是真实墙钟，
        // 与 `base` 相差毫秒级，所以下面的比较留一分钟余量。
        let base = chrono::Utc::now().timestamp_millis();

        // 第一次：正常运行一次并正常收尾，让 last_run_at 有值。
        let (trigger, seen) = recording_trigger();
        svc.defer_for_retry(&auto.id, base - 60_000).unwrap();
        tick_once_at(&svc, &trigger, base).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1, "首次应触发");
        let first_run_id = seen.lock().unwrap()[0].clone();

        // 派发失败 → 退避到一分钟之后。`defer_for_retry` 不动
        // last_run_at，所以下一次 due 判定只剩 `next_run_at <= now` 一条。
        svc.record_run_finish(
            &first_run_id,
            RunStatus::FailedToDispatch,
            None,
            Some("503"),
        )
        .unwrap();
        svc.update_after_run(&auto.id).unwrap();
        let retry_at = base + 60_000;
        svc.defer_for_retry(&auto.id, retry_at).unwrap();

        // 退避未到，不该触发。
        tick_once_at(&svc, &trigger, base).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1, "退避未到时不得重试");

        // 退避到期 → 必须再次触发。
        tick_once_at(&svc, &trigger, retry_at).await.unwrap();
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "退避到期后必须能重试；last_run_at 被改动会让这里为 1"
        );
    }

    /// `last_run_at` 被动过 → 退避到期也触发不了。
    ///
    /// 这是上一条测试的反向对照：两条断言的差别只有 `last_run_at` 这一项，
    /// 所以上一条通过时能证明是"不动 `last_run_at`"在承重，而不是别的什么
    /// 条件恰好成立。直接钉 `is_due` 而不去摆数据库——`update_after_run`
    /// 内部会重算 `next_run_at`，靠调用顺序构造对照随时会被覆盖。
    #[test]
    fn test_touching_last_run_at_kills_the_retry() {
        let retry_at = 1_000_000i64;

        // defer_for_retry 之后：next 前移，last 留在原地 → 到期可触发。
        assert!(
            is_due(ScheduleType::Recurring, Some(retry_at), Some(retry_at - 1), retry_at),
            "last_run_at 早于 next_run_at 时退避必须能触发"
        );

        // 若 deferral 顺手把 last_run_at 推到 next_run_at 之后或相等，
        // 重试被静默吞掉，且没有任何报错。
        assert!(
            !is_due(ScheduleType::Recurring, Some(retry_at), Some(retry_at), retry_at),
            "last_run_at == next_run_at 时不得触发（判重条件失效）"
        );
        assert!(
            !is_due(ScheduleType::Recurring, Some(retry_at), Some(retry_at + 1), retry_at + 1),
            "last_run_at 晚于 next_run_at 时不得触发"
        );
    }

    /// `is_due` 的其余边界，逐条钉住而不是靠端到端测试间接覆盖。
    #[test]
    fn test_is_due_boundaries() {
        let now = 1_000_000i64;
        // next 未到 → 不触发（退避生效中）。
        assert!(!is_due(ScheduleType::Recurring, Some(now + 1), None, now));
        // next 未到，即便 last 更早也不触发。
        assert!(!is_due(ScheduleType::Recurring, Some(now + 1), Some(0), now));
        // next 为空 → 永不触发。
        assert!(!is_due(ScheduleType::Recurring, None, None, now));
        // 首次运行（last 为空）且已到期 → 触发。
        assert!(is_due(ScheduleType::Recurring, Some(now), None, now));
        // 已触发过同一槽位（last >= next）→ 不重复触发。
        assert!(!is_due(ScheduleType::Recurring, Some(now), Some(now), now));

        // Once：只需 last 为空。已跑过的 one-shot 不再触发，
        // 且它不依赖 next_run_at 的相对次序。
        assert!(is_due(ScheduleType::Once, Some(now), None, now));
        assert!(!is_due(ScheduleType::Once, Some(now), Some(0), now));
        assert!(!is_due(ScheduleType::Once, Some(now + 1), None, now));
    }
}
