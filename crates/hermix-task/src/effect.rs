//! Effect Sandwich 持久效果日志（P3，设计 §3.4）。
//!
//! 工具执行三明治：`intent`（要跑这个工具了）→ 事务外执行 →
//! `outcome`（结果是什么）。两笔都持久化到 SQLite，崩溃后
//! 「有 intent 无 outcome」的效果才可判定，而不是凭空猜测。
//!
//! 与 `TaskStatus`（Pending→InProgress→…）正交：status 描述任务
//! 生命周期，效果日志描述任务内每次工具调用的持久痕迹。

use std::path::Path;

use rusqlite::OptionalExtension;

/// 工具意图：执行前持久化。`idempotent` 来自工具 schema 声明，
/// 决定崩溃恢复时重放还是标记 needs_review。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolIntent {
    pub id: String,
    pub task_id: String,
    pub tool: String,
    pub args_json: String,
    pub idempotent: bool,
    pub created_at: i64,
}

/// 工具结果：执行后持久化。`status` 为成功/失败/超时三值。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolOutcome {
    pub id: String,
    pub intent_id: String,
    /// "ok" | "error" | "timeout"
    pub status: String,
    /// 截断后的结果摘要（防 ctx 爆炸，完整结果走既有工具输出路径）。
    pub summary: String,
    pub created_at: i64,
}

/// 恢复判定：有 intent 无 outcome 的效果在崩溃后属于哪种状态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum RecoveryAction {
    /// 工具声明幂等 → 按幂等语义重放。
    Replay(ToolIntent),
    /// 非幂等 → 效果可能已发生，交用户裁决，不自动重放。
    NeedsReview(ToolIntent),
}

#[derive(Debug, thiserror::Error)]
pub enum EffectLogError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 效果日志仓库。与 `TaskRepository` 同库（sessions.db，WAL）。
/// 无状态——每次操作开新连接，模式与 TaskRepository 一致。
pub struct EffectLog {
    db_path: std::path::PathBuf,
}

fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, EffectLogError> {
    let conn = rusqlite::Connection::open(path).map_err(|e| {
        EffectLogError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!("open effects db: {e}").into()),
        ))
    })?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
    Ok(conn)
}

impl EffectLog {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, EffectLogError>,
    ) -> Result<T, EffectLogError> {
        let mut conn = open_with_wal(&self.db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tool_intents (
                id          TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                tool        TEXT NOT NULL,
                args_json   TEXT NOT NULL,
                idempotent  INTEGER NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_intents_task ON tool_intents(task_id);
            CREATE TABLE IF NOT EXISTS tool_outcomes (
                id          TEXT PRIMARY KEY,
                intent_id   TEXT NOT NULL UNIQUE,
                status      TEXT NOT NULL,
                summary     TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );",
        )?;
        f(&mut conn)
    }

    /// 持久化 intent。必须在工具执行**之前**落库（三明治第一笔）。
    pub fn record_intent(&self, intent: &ToolIntent) -> Result<(), EffectLogError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tool_intents (id, task_id, tool, args_json, idempotent, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    intent.id,
                    intent.task_id,
                    intent.tool,
                    intent.args_json,
                    intent.idempotent as i64,
                    intent.created_at
                ],
            )?;
            Ok(())
        })
    }

    /// 持久化 outcome。必须在工具执行**之后**落库（三明治第二笔）。
    /// intent_id 唯一约束保证一笔 intent 至多一笔 outcome。
    pub fn record_outcome(&self, outcome: &ToolOutcome) -> Result<(), EffectLogError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tool_outcomes (id, intent_id, status, summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    outcome.id,
                    outcome.intent_id,
                    outcome.status,
                    outcome.summary,
                    outcome.created_at
                ],
            )?;
            Ok(())
        })
    }

    fn row_to_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolIntent> {
        Ok(ToolIntent {
            id: row.get(0)?,
            task_id: row.get(1)?,
            tool: row.get(2)?,
            args_json: row.get(3)?,
            idempotent: row.get::<_, i64>(4)? != 0,
            created_at: row.get(5)?,
        })
    }

    /// 崩溃恢复扫描（设计 §3.4）：找「有 intent 无 outcome」的效果，
    /// 按幂等性给出重放或 needs_review 判定。一个调用返回全部待裁决项。
    pub fn scan_unfinished(&self) -> Result<Vec<RecoveryAction>, EffectLogError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.id, i.task_id, i.tool, i.args_json, i.idempotent, i.created_at
                 FROM tool_intents i
                 LEFT JOIN tool_outcomes o ON o.intent_id = i.id
                 WHERE o.intent_id IS NULL
                 ORDER BY i.created_at",
            )?;
            let rows = stmt.query_map([], Self::row_to_intent)?;
            let mut out = Vec::new();
            for r in rows {
                let intent = r?;
                out.push(if intent.idempotent {
                    RecoveryAction::Replay(intent)
                } else {
                    RecoveryAction::NeedsReview(intent)
                });
            }
            Ok(out)
        })
    }

    /// 恢复裁决后补录 outcome（needs_review 用户裁决「已发生」时也走这里，
    /// 让 intent 不再悬空）。intent 不存在时静默成功——恢复路径只关心
    /// 最终没有悬空 intent，幂等。
    pub fn resolve_intent(&self, intent_id: &str, status: &str, summary: &str) -> Result<(), EffectLogError> {
        let outcome = ToolOutcome {
            id: format!("out-{}", uuid::Uuid::new_v4().simple()),
            intent_id: intent_id.to_string(),
            status: status.to_string(),
            summary: summary.to_string(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.record_outcome(&outcome)
    }
}

pub fn new_intent_id() -> String {
    format!("int-{}", uuid::Uuid::new_v4().simple())
}

pub fn new_outcome_id() -> String {
    format!("out-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> EffectLog {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        // keep() 后 TempDir 不再自动清理，测试进程退出后由系统临时目录回收。
        EffectLog::new(dir.join("effects.db"))
    }

    fn intent(idempotent: bool) -> ToolIntent {
        ToolIntent {
            id: new_intent_id(),
            task_id: "task-1".into(),
            tool: "terminal".into(),
            args_json: r#"{"cmd":"rm -rf tmp"}"#.into(),
            idempotent,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn sandwich_roundtrip() {
        let log = log();
        let i = intent(true);
        log.record_intent(&i).expect("intent");
        assert!(log.scan_unfinished().expect("scan").len() == 1);
        log.record_outcome(&ToolOutcome {
            id: new_outcome_id(),
            intent_id: i.id.clone(),
            status: "ok".into(),
            summary: "done".into(),
            created_at: chrono::Utc::now().timestamp(),
        })
        .expect("outcome");
        assert!(log.scan_unfinished().expect("scan").is_empty());
    }

    #[test]
    fn recovery_classifies_by_idempotency() {
        let log = log();
        let idem = intent(true);
        let non_idem = intent(false);
        log.record_intent(&idem).expect("idem");
        log.record_intent(&non_idem).expect("non-idem");
        let actions = log.scan_unfinished().expect("scan");
        assert_eq!(actions.len(), 2);
        assert!(actions.contains(&RecoveryAction::Replay(idem)));
        assert!(actions.contains(&RecoveryAction::NeedsReview(non_idem)));
    }

    #[test]
    fn resolve_closes_unfinished_intent() {
        let log = log();
        let i = intent(false);
        log.record_intent(&i).expect("intent");
        log.resolve_intent(&i.id, "ok", "user confirmed effect happened")
            .expect("resolve");
        assert!(log.scan_unfinished().expect("scan").is_empty());
    }

    #[test]
    fn double_outcome_rejected_by_unique_constraint() {
        let log = log();
        let i = intent(true);
        log.record_intent(&i).expect("intent");
        let mk = |status: &str| ToolOutcome {
            id: new_outcome_id(),
            intent_id: i.id.clone(),
            status: status.into(),
            summary: String::new(),
            created_at: 0,
        };
        log.record_outcome(&mk("ok")).expect("first");
        assert!(log.record_outcome(&mk("error")).is_err(), "intent_id UNIQUE");
    }
}
