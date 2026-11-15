//! TaskRecord 持久状态机（L2，设计 §3.3/§3.4）。
//!
//! 设计里的 `TaskRecord` 是任务生命周期的权威字段——UI 读它不推断，
//! 崩溃恢复靠 `scan_unfinished` 判定。与现有 `Task`（编排引擎域对象）
//! 正交：Task 描述"编排引擎在做什么"，TaskRecord 描述"任务本身处于
//! 什么状态 + 结果是什么"。
//!
//! 持久层与 `EffectLog` 同库（sessions.db，WAL）。状态机转移：
//!
//! ```text
//! Pending ──▶ Running ──▶ Terminal
//!                     └─▶ Terminal (Orphaned / Faulted)
//! ```
//!
//! `Pending` / `Running` 携带 `checkpoint`（恢复上下文），`Terminal`
//! 携带 `outcome`（终态结果）。崩溃恢复：扫 `task_records` 里所有
//! `status = 'running'`，结合 `EffectLog.scan_unfinished` 的判定
//! 决定是 Revert→Pending、Keep→Terminal(Orphaned) 还是 Faulted。

use std::path::Path;

use rusqlite::OptionalExtension;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 状态机
// ---------------------------------------------------------------------------

/// 恢复上下文：Pending/Running 时记录"进行到哪一步"，崩溃后决定
/// 能否降级回 Pending 重跑。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    /// 恢复时从哪里续跑（语义由调用方定，当前留空 = 整任务重跑）。
    pub from_step: Option<String>,
    /// 快照时间（epoch ms）。
    pub at: i64,
}

/// 终态结果（设计 §3.3 `TaskOutcome`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskOutcome {
    /// 正常完成。
    Completed { result: Option<Value> },
    /// 失败（携带错误信息 + 可能的部分结果）。
    Failed { error: String, result: Option<Value> },
    /// 用户中止。
    Aborted { reason: Option<String> },
    /// 崩溃后无法归属（重启时找不到对应的 running 记录，或记录已损坏）。
    Orphaned { reason: String },
    /// 内部错误（任务运行时自身故障）。
    Faulted { error: String },
}

impl TaskOutcome {
    /// 终态标记：只有 Terminal 才返回 Some，用于 UI 判断"任务已结束"。
    pub fn is_terminal(&self) -> bool {
        true
    }
}

/// 任务持久状态（设计 §3.3 `TaskState`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskState {
    Pending { checkpoint: Checkpoint },
    Running { checkpoint: Checkpoint },
    Terminal { outcome: TaskOutcome },
}

impl TaskState {
    /// 状态字符串形式（落库用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending { .. } => "pending",
            TaskState::Running { .. } => "running",
            TaskState::Terminal { .. } => "terminal",
        }
    }

    /// 终态结果（非 Terminal 返回 None）。
    pub fn outcome(&self) -> Option<&TaskOutcome> {
        match self {
            TaskState::Terminal { outcome } => Some(outcome),
            _ => None,
        }
    }

    /// 恢复上下文（Pending/Running 才有）。
    pub fn checkpoint(&self) -> Option<&Checkpoint> {
        match self {
            TaskState::Pending { checkpoint } | TaskState::Running { checkpoint } => {
                Some(checkpoint)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// TaskRecord
// ---------------------------------------------------------------------------

/// 任务持久记录（设计 §3.3 `TaskRecord` 对齐版）。
///
/// 与 `EntryRecord.by_task_id` 对应：会话 entry 标记归属哪个任务，
/// 本记录标记任务本身的状态与结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRecord {
    /// 任务 id（与 `tasks` 表同 id，UUID v4）。
    pub id: String,
    /// 归属会话（与 `hermix-session` 的 `conversation_id` 对齐）。
    pub conversation_id: String,
    /// 当前状态。
    pub state: TaskState,
    /// 记录更新时间（epoch ms）。
    pub updated_at: i64,
}

impl TaskRecord {
    /// 构造 Pending 记录（任务刚创建，未开始执行）。
    pub fn new_pending(id: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            conversation_id: conversation_id.into(),
            state: TaskState::Pending {
                checkpoint: Checkpoint {
                    from_step: None,
                    at: chrono::Utc::now().timestamp_millis(),
                },
            },
            updated_at: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 构造 Running 记录（任务开始执行）。
    pub fn new_running(id: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            conversation_id: conversation_id.into(),
            state: TaskState::Running {
                checkpoint: Checkpoint {
                    from_step: None,
                    at: chrono::Utc::now().timestamp_millis(),
                },
            },
            updated_at: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 构造 Terminal 记录（任务结束）。
    pub fn terminal(
        id: impl Into<String>,
        conversation_id: impl Into<String>,
        outcome: TaskOutcome,
    ) -> Self {
        Self {
            id: id.into(),
            conversation_id: conversation_id.into(),
            state: TaskState::Terminal { outcome },
            updated_at: chrono::Utc::now().timestamp_millis(),
        }
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TaskRecordError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Task record not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// 持久层
// ---------------------------------------------------------------------------

/// TaskRecord 持久仓库。与 `EffectLog` 同库（sessions.db，WAL）。
/// 无状态——每次操作开新连接，模式与 `EffectLog` / `TaskRepository` 一致。
pub struct TaskRecordStore {
    db_path: std::path::PathBuf,
}

fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, TaskRecordError> {
    hermix_store::open_connection(path, hermix_store::OpenMode::Write).map_err(|e| {
        TaskRecordError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!("open task records db: {e}").into()),
        ))
    })
}

impl TaskRecordStore {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, TaskRecordError>,
    ) -> Result<T, TaskRecordError> {
        let mut conn = open_with_wal(&self.db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_records (
                id               TEXT PRIMARY KEY,
                conversation_id  TEXT NOT NULL,
                state            TEXT NOT NULL,
                checkpoint_json  TEXT,
                outcome_json     TEXT,
                updated_at       INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_task_records_state ON task_records(state);
            CREATE INDEX IF NOT EXISTS idx_task_records_conv ON task_records(conversation_id);",
        )?;
        f(&mut conn)
    }

    /// 写入一条 TaskRecord（覆盖式 upsert）。同一任务的状态转移
    /// 通过多次调用实现，每次覆盖前一行。
    pub fn save(&self, record: &TaskRecord) -> Result<(), TaskRecordError> {
        let checkpoint_json = record
            .state
            .checkpoint()
            .map(|cp| serde_json::to_string(cp).map_err(TaskRecordError::Json))
            .transpose()?;
        let outcome_json = record
            .state
            .outcome()
            .map(|oc| serde_json::to_string(oc).map_err(TaskRecordError::Json))
            .transpose()?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO task_records
                     (id, conversation_id, state, checkpoint_json, outcome_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    record.id,
                    record.conversation_id,
                    record.state.as_str(),
                    checkpoint_json,
                    outcome_json,
                    record.updated_at
                ],
            )?;
            Ok(())
        })
    }

    /// 读单条记录。
    pub fn get(&self, id: &str) -> Result<Option<TaskRecord>, TaskRecordError> {
        self.with_conn(|conn| {
            let row: Option<(String, String, String, Option<String>, Option<String>, i64)> =
                conn.query_row(
                    "SELECT id, conversation_id, state, checkpoint_json, outcome_json, updated_at
                     FROM task_records WHERE id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?;
            row.map(|r| Self::row_to_record(r)).transpose()
        })
    }

    /// 列出某会话下的所有 TaskRecord（按更新时间倒序）。
    pub fn list_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TaskRecord>, TaskRecordError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, state, checkpoint_json, outcome_json, updated_at
                 FROM task_records WHERE conversation_id = ?1
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([conversation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(Self::row_to_record(r?)?);
            }
            Ok(out)
        })
    }

    /// 崩溃恢复扫描（设计 §3.4）：找所有 `state = 'running'` 的记录，
    /// 结合 `EffectLog.scan_unfinished` 的判定做恢复。
    ///
    /// 返回的 `Pending` 记录是"需要重新进入"的任务；`Terminal(Orphaned)`
    /// 是"找不到对应 EffectLog intent"的孤立记录。
    ///
    /// 注意：本方法**不修改**任何记录，只读。实际的状态转移由调用方
    /// 通过 `save` 实现。
    pub fn scan_unfinished(
        &self,
    ) -> Result<Vec<TaskRecord>, TaskRecordError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, state, checkpoint_json, outcome_json, updated_at
                 FROM task_records WHERE state = 'running'
                 ORDER BY updated_at",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(Self::row_to_record(r?)?);
            }
            Ok(out)
        })
    }

    fn row_to_record(
        (id, conversation_id, state_str, checkpoint_str, outcome_str, updated_at): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
        ),
    ) -> Result<TaskRecord, TaskRecordError> {
        let state = match state_str.as_str() {
            "pending" => TaskState::Pending {
                checkpoint: checkpoint_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Checkpoint {
                        from_step: None,
                        at: 0,
                    }),
            },
            "running" => TaskState::Running {
                checkpoint: checkpoint_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Checkpoint {
                        from_step: None,
                        at: 0,
                    }),
            },
            "terminal" => TaskState::Terminal {
                outcome: outcome_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(TaskOutcome::Orphaned {
                        reason: "outcome_json missing/corrupt".into(),
                    }),
            },
            other => {
                tracing::warn!(
                    "task_records: unknown state '{other}', treating as Orphaned"
                );
                TaskState::Terminal {
                    outcome: TaskOutcome::Orphaned {
                        reason: format!("unknown state '{other}'"),
                    },
                }
            }
        };
        Ok(TaskRecord {
            id,
            conversation_id,
            state,
            updated_at,
        })
    }
}

// ---------------------------------------------------------------------------
// 崩溃恢复协议（设计 §3.4）
// ---------------------------------------------------------------------------

use crate::effect::{EffectLog, RecoveryAction};

/// 单任务的恢复决策（设计 §3.4 判定表）。纯函数，只读任务与
/// 其未决效果，状态转移由 [`recover`] 负责执行。
///
/// - `Noop`        —— 任务已是终态/Pending，不参与恢复
/// - `Reopen`      —— 可迁移：无未决 effect，降级 Pending，保留 checkpoint 重跑
/// - `Replay`      —— 有且仅有幂等未决 effect：按幂等语义重放
/// - `NeedsReview` —— 有非幂等未决 effect：效果可能已发生，交用户裁决，
///                   不自动重放；任务落 Orphaned 终态（T5），悬空 intent
///                   由 `recover` 封闭
///
/// （设计 §3.4 判定表里的 `Orphaned` 终态由 `recover` 在 effect-scan
/// 失败时直接生成，不属于 `decide` 的判定域。）
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskRecoveryDecision {
    Noop,
    Reopen,
    Replay,
    NeedsReview,
}

/// 恢复计划：一次崩溃恢复扫描的产物。调用方按 `decision` 分发，
/// 并按 `resolved_intent_ids` 调 `EffectLog::resolve_intent` 封闭
/// 悬空 intent（needs_review 用户裁决「已发生」/ 任务判 Orphaned
/// 时都要走这里，intent 不留空）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoveryPlan {
    pub task_id: String,
    pub decision: TaskRecoveryDecision,
    /// 本任务当前悬空的 intent id 集合（供封闭审计用）。
    pub hanging_intent_ids: Vec<String>,
    /// `recover` 实际 resolve 掉的 intent（NeedsReview/Orphaned 分支）。
    pub resolved_intent_ids: Vec<String>,
}

/// 联合恢复协议入口：扫所有 `state='running'` 的 TaskRecord，对每个
/// 任务查 EffectLog 的 task 级未决效果，按设计判定表决策 + 执行状态
/// 转移。一个调用返回全部计划；实际 effect 重放/用户裁决由调用方
/// 按 `decision` 二次分发（本协议只负责"判定 + 任务状态落库 +
/// 封闭悬空 intent"，不碰 effect 本身的重放副作用）。
pub fn recover(store: &TaskRecordStore, effects: &EffectLog) -> Vec<RecoveryPlan> {
    let unfinished = match store.scan_unfinished() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "recover: task_records scan failed, skipping");
            return Vec::new();
        }
    };
    let mut plans = Vec::new();
    for record in unfinished {
        let task_id = record.id.clone();
        let effects_scan = match effects.scan_unfinished_for_task(&task_id) {
            Ok(v) => v,
            Err(e) => {
                // EffectLog 扫描失败 = 效果面不可信，保守判 Orphaned，
                // 不做任何自动重放。
                tracing::warn!(
                    task_id = %task_id,
                    error = %e,
                    "recover: effect scan failed, marking Orphaned"
                );
                let mut orphan = record.clone();
                orphan.state = TaskState::Terminal {
                    outcome: TaskOutcome::Orphaned {
                        reason: format!("effect scan failed: {e}"),
                    },
                };
                orphan.updated_at = chrono::Utc::now().timestamp_millis();
                if let Err(e) = store.save(&orphan) {
                    tracing::warn!(task_id = %task_id, error = %e, "recover: save Orphaned failed");
                }
                plans.push(RecoveryPlan {
                    task_id,
                    decision: TaskRecoveryDecision::NeedsReview,
                    hanging_intent_ids: Vec::new(),
                    resolved_intent_ids: Vec::new(),
                });
                continue;
            }
        };
        let decision = TaskRecoveryDecision::decide(&record, &effects_scan);
        let hanging_ids: Vec<String> = effects_scan
            .iter()
            .map(|a| match a {
                RecoveryAction::Replay(i) | RecoveryAction::NeedsReview(i) => i.id.clone(),
            })
            .collect();

        match decision {
            TaskRecoveryDecision::Reopen => {
                let mut reset = record.clone();
                reset.state = TaskState::Pending {
                    checkpoint: Checkpoint {
                        from_step: None,
                        at: chrono::Utc::now().timestamp_millis(),
                    },
                };
                reset.updated_at = chrono::Utc::now().timestamp_millis();
                if let Err(e) = store.save(&reset) {
                    tracing::warn!(task_id = %task_id, error = %e, "recover: save Reopen failed");
                }
            }
            TaskRecoveryDecision::Replay => {
                // 保持 Running，等调用方按幂等语义重放。
            }
            TaskRecoveryDecision::NeedsReview => {
                // 封闭本任务悬空 intent（用户裁决"已发生"时 resolve_intent 走
                // EffectLog，这里只标记"已交给上层裁决"，不重放）。
                let mut resolved = Vec::new();
                for id in &hanging_ids {
                    if effects.resolve_intent(id, "needs_review", "recovery deferred to user").is_ok()
                    {
                        resolved.push(id.clone());
                    }
                }
                let mut term = record.clone();
                term.state = TaskState::Terminal {
                    outcome: TaskOutcome::Orphaned {
                        reason: "non-idempotent effect pending, user review required".into(),
                    },
                };
                term.updated_at = chrono::Utc::now().timestamp_millis();
                if let Err(e) = store.save(&term) {
                    tracing::warn!(task_id = %task_id, error = %e, "recover: save Terminal failed");
                }
                plans.push(RecoveryPlan {
                    task_id,
                    decision,
                    hanging_intent_ids: hanging_ids,
                    resolved_intent_ids: resolved,
                });
                continue;
            }
            TaskRecoveryDecision::Noop => {}
        }
        plans.push(RecoveryPlan {
            task_id,
            decision,
            hanging_intent_ids: hanging_ids,
            resolved_intent_ids: Vec::new(),
        });
    }
    plans
}

impl TaskRecoveryDecision {
    /// 设计 §3.4 判定表。`effects` 必须是该任务的未决效果集
    /// （`EffectLog::scan_unfinished_for_task` 的产物），避免跨任务误判。
    pub fn decide(record: &TaskRecord, effects: &[RecoveryAction]) -> Self {
        match &record.state {
            TaskState::Terminal { .. } => Self::Noop,
            TaskState::Pending { .. } => Self::Noop,
            TaskState::Running { .. } => {
                if effects.is_empty() {
                    Self::Reopen
                } else if effects.iter().any(|a| matches!(a, RecoveryAction::NeedsReview(_))) {
                    Self::NeedsReview
                } else {
                    Self::Replay
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{new_intent_id, ToolIntent};

    fn store() -> (TaskRecordStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskRecordStore::new(dir.path().join("tasks.db"));
        (store, dir)
    }

    fn pending_record(id: &str) -> TaskRecord {
        TaskRecord::new_pending(id, "conv-1")
    }

    fn running_record(id: &str) -> TaskRecord {
        TaskRecord::new_running(id, "conv-1")
    }

    fn terminal_record(id: &str, outcome: TaskOutcome) -> TaskRecord {
        TaskRecord::terminal(id, "conv-1", outcome)
    }

    fn log_with_pending_intent(task_id: &str, idempotent: bool) -> crate::EffectLog {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = crate::EffectLog::new(dir.path().join("effects.db"));
        log.record_intent(&ToolIntent {
            id: new_intent_id(),
            task_id: task_id.to_string(),
            tool: "terminal".into(),
            args_json: r#"{"command":"echo hi"}"#.into(),
            idempotent,
            created_at: chrono::Utc::now().timestamp(),
        })
        .expect("record intent");
        // keep() 防 TempDir drop 时目录消失（EffectLog 只存路径，GC 后失效）。
        drop(dir.keep());
        log
    }

    // ----- decide 纯函数（设计 §3.4 判定表逐行钉死）-----

    #[test]
    fn decide_migratable_running_is_reopen() {
        let d = TaskRecoveryDecision::decide(&running_record("t-1"), &[]);
        assert_eq!(d, TaskRecoveryDecision::Reopen);
    }

    #[test]
    fn decide_pending_is_noop() {
        let d = TaskRecoveryDecision::decide(&pending_record("t-1"), &[]);
        assert_eq!(d, TaskRecoveryDecision::Noop);
    }

    #[test]
    fn decide_replays_only_idempotent_effects() {
        let log = log_with_pending_intent("t-1", true);
        let actions = log.scan_unfinished_for_task("t-1").expect("scan");
        let d = TaskRecoveryDecision::decide(&running_record("t-1"), &actions);
        assert_eq!(d, TaskRecoveryDecision::Replay);
    }

    #[test]
    fn decide_needs_review_when_non_idempotent_effect_hangs() {
        let log = log_with_pending_intent("t-1", false);
        let actions = log.scan_unfinished_for_task("t-1").expect("scan");
        let d = TaskRecoveryDecision::decide(&running_record("t-1"), &actions);
        assert_eq!(d, TaskRecoveryDecision::NeedsReview);
    }

    #[test]
    fn decide_needs_review_wins_over_replay() {
        let log = log_with_pending_intent("t-1", true);
        log.record_intent(&ToolIntent {
            id: new_intent_id(),
            task_id: "t-1".into(),
            tool: "write_file".into(),
            args_json: r#"{}"#.into(),
            idempotent: false,
            created_at: chrono::Utc::now().timestamp(),
        })
        .expect("second intent");
        let actions = log.scan_unfinished_for_task("t-1").expect("scan");
        let d = TaskRecoveryDecision::decide(&running_record("t-1"), &actions);
        assert_eq!(d, TaskRecoveryDecision::NeedsReview);
    }

    #[test]
    fn decide_terminal_is_noop_even_with_hanging_effects() {
        let log = log_with_pending_intent("t-1", false);
        let actions = log.scan_unfinished_for_task("t-1").expect("scan");
        let rec = terminal_record("t-1", TaskOutcome::Completed { result: None });
        let d = TaskRecoveryDecision::decide(&rec, &actions);
        assert_eq!(d, TaskRecoveryDecision::Noop);
    }

    // ----- recover 编排（状态转移 + 悬空 intent 封闭）-----

    #[test]
    fn recover_reopen_resets_record_to_pending() {
        let (store, _dir) = store();
        store.save(&running_record("t-1")).expect("save");
        let plan = recover(&store, &crate::EffectLog::new(_dir.path().join("unused.db")));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].decision, TaskRecoveryDecision::Reopen);
        assert!(store
            .get("t-1")
            .expect("get")
            .unwrap()
            .state
            .checkpoint()
            .is_some());
    }

    #[test]
    fn recover_noop_leaves_terminal_records_untouched() {
        let (store, _dir) = store();
        store
            .save(&terminal_record("t-1", TaskOutcome::Completed { result: None }))
            .expect("save");
        let plan = recover(&store, &crate::EffectLog::new(_dir.path().join("unused.db")));
        assert!(plan.is_empty(), "terminal task must not enter recovery plan");
    }

    #[test]
    fn recover_needs_review_resolves_hanging_intents_and_marks_terminal() {
        let (store, _dir) = store();
        store.save(&running_record("t-1")).expect("save");
        let log = log_with_pending_intent("t-1", false);
        log.record_intent(&ToolIntent {
            id: new_intent_id(),
            task_id: "t-9".into(),
            tool: "terminal".into(),
            args_json: r#"{}"#.into(),
            idempotent: false,
            created_at: chrono::Utc::now().timestamp(),
        })
        .expect("other-task intent must not be touched");

        let plan = recover(&store, &log);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].decision, TaskRecoveryDecision::NeedsReview);
        assert_eq!(plan[0].resolved_intent_ids.len(), 1);

        // 本任务 intent 已封闭（resolve），其他任务 intent 保持悬空。
        let t1 = log.scan_unfinished_for_task("t-1").expect("t1 scan");
        assert!(t1.is_empty(), "needs_review intents must be resolved");
        let t9 = log.scan_unfinished_for_task("t-9").expect("t9 scan");
        assert_eq!(t9.len(), 1, "other tasks' intents must stay hanging");

        // 记录已落 Terminal(Orphaned)——非幂等未决效果的 T5 终态。
        let rec = store.get("t-1").expect("get").unwrap();
        match rec.state {
            TaskState::Terminal {
                outcome: TaskOutcome::Orphaned { .. },
            } => {}
            other => panic!("expected Orphaned(T5) terminal, got {other:?}"),
        }
    }
}
