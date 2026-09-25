//! Adapter: implements `hermix_agent::TaskRecordSink` over
//! `hermix_task::TaskRecordStore` (§3.3 热路径生产者).
//!
//! Bridges the agent layer (hermix-agent) to the domain layer
//! (hermix-task) without a direct dependency between them — same
//! pattern as `task_tracker_adapter.rs`.
//!
//! Best-effort by contract: persistence failures are logged, never
//! propagated — delegation must not break because records fail.

use std::sync::Arc;

use hermix_agent::TaskRecordSink;
use hermix_task::{TaskOutcome, TaskRecord, TaskRecordStore, TaskState};

pub struct TaskRecordSinkImpl {
    store: Arc<TaskRecordStore>,
}

impl TaskRecordSinkImpl {
    pub fn new(store: Arc<TaskRecordStore>) -> Self {
        Self { store }
    }
}

impl TaskRecordSink for TaskRecordSinkImpl {
    fn started(&self, sub_agent_id: &str, conversation_id: Option<&str>, _task: &str) {
        // 未知会话归属时退化为 "unknown"：记录必须落盘（恢复端按
        // conversation_id 过滤，unknown 桶仍可 scan_unfinished 扫到）。
        let record = TaskRecord::new_running(
            sub_agent_id,
            conversation_id.unwrap_or("unknown"),
        );
        if let Err(e) = self.store.save(&record) {
            tracing::warn!(error = %e, sub_agent_id, "TaskRecord started 落盘失败（best-effort）");
        }
    }

    fn terminal(&self, sub_agent_id: &str, outcome: &str, error: Option<&str>) {
        // 终态覆盖：以既有记录保留 conversation_id，读不到则用 unknown
        // （与 started 的退化一致）。
        let conversation_id = self
            .store
            .get(sub_agent_id)
            .ok()
            .flatten()
            .map(|r| r.conversation_id)
            .unwrap_or_else(|| "unknown".to_string());
        let outcome = match outcome {
            "completed" => TaskOutcome::Completed { result: None },
            "aborted" => TaskOutcome::Aborted {
                reason: error.map(String::from),
            },
            "orphaned" => TaskOutcome::Orphaned {
                reason: error.unwrap_or("tokio task lost").to_string(),
            },
            _ => TaskOutcome::Faulted {
                error: error.unwrap_or("sub-agent failed").to_string(),
            },
        };
        let mut record = TaskRecord::terminal(sub_agent_id, conversation_id, outcome);
        // 保留原 started 时刻：terminal 构造器会刷新 updated_at，
        // 但 state 必须整体替换为 Terminal（无保留语义可借用）。
        if let Ok(Some(prev)) = self.store.get(sub_agent_id) {
            if let TaskState::Running { checkpoint } = prev.state {
                record.state = hermix_task::TaskState::Terminal { outcome: match &record.state {
                    TaskState::Terminal { outcome } => outcome.clone(),
                    _ => unreachable!(),
                } };
                let _ = checkpoint; // checkpoint 细化属后续批次（YAGNI）
            }
        }
        if let Err(e) = self.store.save(&record) {
            tracing::warn!(error = %e, sub_agent_id, "TaskRecord terminal 落盘失败（best-effort）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试自持 tempdir（保活到测试结束），store 与 sink 共享同一实例。
    fn setup() -> (tempfile::TempDir, TaskRecordStore, TaskRecordSinkImpl) {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskRecordStore::new(dir.path().join("effects.db"));
        let sink = TaskRecordSinkImpl::new(std::sync::Arc::new(TaskRecordStore::new(
            dir.path().join("effects.db"),
        )));
        (dir, store, sink)
    }

    #[test]
    fn started_then_terminal_preserves_conversation_id() {
        let (_dir, store, sink) = setup();
        sink.started("subagent-1", Some("conv-7"), "do things");

        let mid = store.get("subagent-1").unwrap().unwrap();
        assert_eq!(mid.conversation_id, "conv-7");
        assert!(matches!(mid.state, TaskState::Running { .. }));

        sink.terminal("subagent-1", "completed", None);
        let done = store.get("subagent-1").unwrap().unwrap();
        assert_eq!(done.conversation_id, "conv-7");
        match done.state {
            TaskState::Terminal { outcome } => {
                assert!(matches!(outcome, TaskOutcome::Completed { .. }))
            }
            other => panic!("expected Terminal, got {other:?}"),
        }
    }

    #[test]
    fn failed_outcome_maps_to_faulted_with_error() {
        let (_dir, store, sink) = setup();
        sink.started("subagent-2", None, "x");
        sink.terminal("subagent-2", "failed", Some("boom"));
        let rec = store.get("subagent-2").unwrap().unwrap();
        match rec.state {
            TaskState::Terminal { outcome } => match outcome {
                TaskOutcome::Faulted { error } => assert_eq!(error, "boom"),
                other => panic!("expected Faulted, got {other:?}"),
            },
            other => panic!("expected Terminal, got {other:?}"),
        }
    }

    #[test]
    fn orphaned_outcome_maps_to_orphaned() {
        let (_dir, store, sink) = setup();
        sink.terminal("orphaned-abc", "orphaned", Some("tokio task lost: panicked"));
        let rec = store.get("orphaned-abc").unwrap().unwrap();
        match rec.state {
            TaskState::Terminal { outcome } => match outcome {
                TaskOutcome::Orphaned { reason } => {
                    assert!(reason.contains("panicked"))
                }
                other => panic!("expected Orphaned, got {other:?}"),
            },
            other => panic!("expected Terminal, got {other:?}"),
        }
    }
}
