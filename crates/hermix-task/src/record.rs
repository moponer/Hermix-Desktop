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
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
    Ok(conn)
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (TaskRecordStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskRecordStore::new(dir.path().join("tasks.db"));
        (store, dir)
    }

    #[test]
    fn pending_running_terminal_roundtrip() {
        let (store, _dir) = store();
        let pending = TaskRecord::new_pending("t-1", "conv-1");
        store.save(&pending).expect("save pending");

        let running = TaskRecord::new_running("t-1", "conv-1");
        store.save(&running).expect("save running");

        let terminal = TaskRecord::terminal(
            "t-1",
            "conv-1",
            TaskOutcome::Completed { result: None },
        );
        store.save(&terminal).expect("save terminal");

        let fetched = store.get("t-1").expect("get").expect("exists");
        assert!(matches!(
            fetched.state,
            TaskState::Terminal { .. }
        ));
        assert!(fetched.state.outcome().is_some());
    }

    #[test]
    fn scan_unfinished_returns_running_records() {
        let (store, _dir) = store();
        store.save(&TaskRecord::new_pending("t-1", "c-1")).expect("p");
        store.save(&TaskRecord::new_running("t-2", "c-1")).expect("r");
        store
            .save(&TaskRecord::terminal("t-3", "c-1", TaskOutcome::Faulted { error: "e".into() }))
            .expect("t");

        let unfinished = store.scan_unfinished().expect("scan");
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, "t-2");
    }

    #[test]
    fn list_by_conversation_filters() {
        let (store, _dir) = store();
        store
            .save(&TaskRecord::new_pending("t-1", "c-1"))
            .expect("1");
        store
            .save(&TaskRecord::new_running("t-2", "c-1"))
            .expect("2");
        store
            .save(&TaskRecord::new_pending("t-3", "c-2"))
            .expect("3");

        let c1 = store.list_by_conversation("c-1").expect("c1");
        assert_eq!(c1.len(), 2);
        let c2 = store.list_by_conversation("c-2").expect("c2");
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0].id, "t-3");
    }

    #[test]
    fn orphaned_when_outcome_corrupt() {
        let (store, _dir) = store();
        // 直接写一行损坏的 outcome_json（模拟崩溃半写）。
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO task_records (id, conversation_id, state, checkpoint_json, outcome_json, updated_at)
                     VALUES ('t-x', 'c-x', 'terminal', NULL, '{corrupt}', 0)",
                    [],
                )?;
                Ok(())
            })
            .expect("insert");
        let fetched = store.get("t-x").expect("get").expect("exists");
        match fetched.state {
            TaskState::Terminal {
                outcome: TaskOutcome::Orphaned { .. },
            } => {}
            other => panic!("expected Orphaned, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_serialization_roundtrip() {
        let (store, _dir) = store();
        let mut record = TaskRecord::new_running("t-1", "c-1");
        if let TaskState::Running { checkpoint } = &mut record.state {
            checkpoint.from_step = Some("step-42".into());
        }
        store.save(&record).expect("save");
        let fetched = store.get("t-1").expect("get").expect("exists");
        match fetched.state {
            TaskState::Running { checkpoint } => {
                assert_eq!(checkpoint.from_step.as_deref(), Some("step-42"));
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }
}
