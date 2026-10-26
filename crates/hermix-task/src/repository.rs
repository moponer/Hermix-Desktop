//! SQLite-backed CRUD for the `tasks` table.
//!
//! Shares the same `sessions.db` file as `SessionPersistence` (铁律 D/WAL).
//! All operations go through [`open_with_wal`] to ensure WAL mode +
//! busy_timeout are set on every connection.
//!
//!

use std::path::{Path, PathBuf};

use hermix_core::{TaskContext, TaskMode, TaskResultRef, TaskStatus};
use rusqlite::OptionalExtension;
use serde_json;

use crate::Task;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TaskRepositoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Task not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// Connection helper (mirrors SessionPersistence::open_with_wal)
// ---------------------------------------------------------------------------

fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, TaskRepositoryError> {
    // 写连接统一走 `hermix_store`（WAL + 5000ms busy_timeout）。此前仓库里
    // 有四份各自实现的"打开 SQLite"，任何一处漂移都会让争用问题只在一半
    // 代码里出现。
    let conn = hermix_store::open_connection(path, hermix_store::OpenMode::Write)
        .map_err(|e| TaskRepositoryError::Io(format!("Failed to open tasks db: {e}")))?;
    // 外键级联是 automation 表的要求，不属于通用 pragma。
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
    Ok(conn)
}

// ---------------------------------------------------------------------------
// TaskRepository
// ---------------------------------------------------------------------------

/// SQLite-backed repository for `Task` entities.
/// Stateless — each method opens a fresh connection to the DB. This
/// matches the pattern used by `SessionPersistence` and avoids
/// connection lifecycle issues in multi-threaded contexts.
pub struct TaskRepository {
    db_path: PathBuf,
}

impl TaskRepository {
    /// Create a new repository pointing at the given SQLite database.
    /// In production, pass `hermix_config::paths::tasks_db_path()`.
    /// In tests, pass a `tempfile` path.
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    /// Ensure the `tasks` table exists (idempotent — also created by
    /// `SessionPersistence::ensure_db`, but this allows `hermix-task`
    /// to be used standalone).
    pub fn ensure_schema(&self) -> Result<(), TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS tasks (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                workspace_path  TEXT NOT NULL,
                mode            TEXT NOT NULL DEFAULT 'craft',
                status          TEXT NOT NULL DEFAULT 'pending',
                parent_task_id  TEXT,
                context_json    TEXT NOT NULL DEFAULT '{}',
                result_refs_json TEXT NOT NULL DEFAULT '[]',
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                session_id      TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);"#,
        )?;
        // Column migration: `CREATE TABLE IF NOT EXISTS` does not add
        // columns to an existing table, and every install that predates
        // the session column has a `tasks` table without it.
        Self::ensure_column(&conn, "tasks", "session_id", "TEXT")?;
        // ★ The session index must be created **after** the column
        // exists. It used to sit in the batch above, i.e. *before*
        // `ensure_column` — and on a table without `session_id` (every
        // pre-existing install, plus every fresh install, because
        // `SessionPersistence::ensure_db` creates `tasks` without the
        // column and runs first) `CREATE INDEX ... ON tasks(session_id)`
        // fails with "no such column". `execute_batch` stops at the
        // first error, so the `ALTER TABLE` never ran and every
        // `task_*` query then failed at runtime.
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);")?;
        Ok(())
    }

    /// Add a column to an existing table if it is not there yet.
    ///
    /// `CREATE TABLE IF NOT EXISTS` never adds columns to a table that
    /// already exists, so a schema change needs its own migration step
    /// for the installs that predate it.
    fn ensure_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        ty: &str,
    ) -> Result<(), TaskRepositoryError> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(());
            }
        }
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
        Ok(())
    }

    /// Insert a new task. Returns the inserted task on success.
    pub fn create(&self, task: &Task) -> Result<(), TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let context_json = serde_json::to_string(&task.context)?;
        let results_json = serde_json::to_string(&task.results)?;
        let mode_str = match task.mode {
            TaskMode::Craft => "craft",
            TaskMode::Plan => "plan",
            TaskMode::Ask => "ask",
        };
        let status_str = match task.status {
            TaskStatus::Pending => "pending",
            TaskStatus::Planning => "planning",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Archived => "archived",
        };
        conn.execute(
            "INSERT INTO tasks (id, title, workspace_path, mode, status, parent_task_id,
                context_json, result_refs_json, created_at, updated_at, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                task.id,
                task.title,
                task.workspace_path,
                mode_str,
                status_str,
                task.parent_task_id,
                context_json,
                results_json,
                task.created_at,
                task.updated_at,
                task.session_id,
            ],
        )?;
        Ok(())
    }

    /// Fetch a task by ID. Returns `Err(NotFound)` if not present.
    pub fn get(&self, id: &str) -> Result<Task, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let task = conn
            .query_row(
                "SELECT id, title, workspace_path, mode, status, parent_task_id,
                        context_json, result_refs_json, created_at, updated_at, session_id
                 FROM tasks WHERE id = ?1",
                [id],
                row_to_task,
            )
            .optional()?;
        task.ok_or(TaskRepositoryError::NotFound(id.to_string()))
    }

    /// Update a task's mutable fields (title, status, context, results,
    /// parent_task_id). The `updated_at` timestamp is refreshed.
    pub fn update(&self, task: &Task) -> Result<(), TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let context_json = serde_json::to_string(&task.context)?;
        let results_json = serde_json::to_string(&task.results)?;
        let mode_str = match task.mode {
            TaskMode::Craft => "craft",
            TaskMode::Plan => "plan",
            TaskMode::Ask => "ask",
        };
        let status_str = match task.status {
            TaskStatus::Pending => "pending",
            TaskStatus::Planning => "planning",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Archived => "archived",
        };
        let now = chrono::Utc::now().timestamp_millis();
        let affected = conn.execute(
            "UPDATE tasks SET title = ?2, mode = ?3, status = ?4, parent_task_id = ?5,
                context_json = ?6, result_refs_json = ?7, updated_at = ?8
             WHERE id = ?1",
            rusqlite::params![
                task.id,
                task.title,
                mode_str,
                status_str,
                task.parent_task_id,
                context_json,
                results_json,
                now,
            ],
        )?;
        if affected == 0 {
            return Err(TaskRepositoryError::NotFound(task.id.clone()));
        }
        Ok(())
    }

    /// Update only the status of a task. Faster than `update` when only
    /// the lifecycle state changes.
    pub fn update_status(
        &self,
        id: &str,
        new_status: TaskStatus,
    ) -> Result<(), TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let status_str = match new_status {
            TaskStatus::Pending => "pending",
            TaskStatus::Planning => "planning",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Archived => "archived",
        };
        let now = chrono::Utc::now().timestamp_millis();
        let affected = conn.execute(
            "UPDATE tasks SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status_str, now],
        )?;
        if affected == 0 {
            return Err(TaskRepositoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Stamp the owning session onto a task that has none yet.
    ///
    /// Only `NULL` owners are claimable — a task already owned by another
    /// session must not change hands by accident (`WHERE session_id IS
    /// NULL` is the whole guard, evaluated by SQLite).
    pub fn claim(&self, id: &str, session_id: &str) -> Result<Task, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let now = chrono::Utc::now().timestamp_millis();
        let affected = conn.execute(
            "UPDATE tasks SET session_id = ?2, updated_at = ?3 \
             WHERE id = ?1 AND session_id IS NULL",
            rusqlite::params![id, session_id, now],
        )?;
        if affected == 0 {
            // Missing, or already owned by someone else. `NotFound` is
            // the honest answer for both: nothing was claimed.
            return Err(TaskRepositoryError::NotFound(id.to_string()));
        }
        self.get(id)
    }

    /// Delete a task by ID (hard delete).
    pub fn delete(&self, id: &str) -> Result<(), TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let affected = conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        if affected == 0 {
            return Err(TaskRepositoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// List tasks with optional status filter. Returns tasks ordered
    /// by `updated_at` descending (most recently active first).
    /// List tasks, optionally filtered by status and by owning session.
    ///
    /// With a session id the result is that session's own tasks **plus**
    /// the still-unclaimed ones (rows written before the session column
    /// existed). Unclaimed rows stay visible so no task silently
    /// disappears from the model's view; the caller tells them apart by
    /// `Task::session_id`.
    ///
    /// Without a session id the list is global — the pre-existing
    /// behaviour, kept for callers that have no session (CLI, cron).
    pub fn list(
        &self,
        status_filter: Option<TaskStatus>,
        session_id: Option<&str>,
    ) -> Result<Vec<Task>, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut tasks = Vec::new();
        let status_str = status_filter.map(|status| match status {
            TaskStatus::Pending => "pending",
            TaskStatus::Planning => "planning",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Archived => "archived",
        });
        let mut stmt = conn.prepare(
            "SELECT id, title, workspace_path, mode, status, parent_task_id,
                    context_json, result_refs_json, created_at, updated_at, session_id
             FROM tasks
             WHERE (?1 IS NULL OR status = ?1)
               AND (?2 IS NULL OR session_id = ?2 OR session_id IS NULL)
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![status_str, session_id], row_to_task)?;
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// List child tasks of a parent task.
    pub fn list_children(&self, parent_task_id: &str) -> Result<Vec<Task>, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, title, workspace_path, mode, status, parent_task_id,
                    context_json, result_refs_json, created_at, updated_at, session_id
             FROM tasks WHERE parent_task_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([parent_task_id], row_to_task)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }
}

// ---------------------------------------------------------------------------
// Row mapper
// ---------------------------------------------------------------------------

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    let mode_str: String = row.get(3)?;
    let status_str: String = row.get(4)?;
    let context_json: String = row.get(6)?;
    let results_json: String = row.get(7)?;

    let mode = match mode_str.as_str() {
        "craft" => TaskMode::Craft,
        "plan" => TaskMode::Plan,
        "ask" => TaskMode::Ask,
        other => {
            tracing::warn!("Unknown task mode '{other}', defaulting to Craft");
            TaskMode::Craft
        }
    };
    let status = match status_str.as_str() {
        "pending" => TaskStatus::Pending,
        "planning" => TaskStatus::Planning,
        "in_progress" => TaskStatus::InProgress,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "archived" => TaskStatus::Archived,
        other => {
            tracing::warn!("Unknown task status '{other}', defaulting to Pending");
            TaskStatus::Pending
        }
    };
    let context: TaskContext = serde_json::from_str(&context_json).unwrap_or_default();
    let results: Vec<TaskResultRef> = serde_json::from_str(&results_json).unwrap_or_default();

    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        workspace_path: row.get(2)?,
        mode,
        status,
        parent_task_id: row.get(5)?,
        context,
        results,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        session_id: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (TaskRepository, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = TaskRepository::new(tmp.path().join("test.db"));
        repo.ensure_schema().unwrap();
        (repo, tmp)
    }

    #[test]
    fn test_create_and_get() {
        let (repo, _tmp) = repo();
        let task = Task::new("Test", "/workspace", TaskMode::Craft);
        repo.create(&task).unwrap();
        let fetched = repo.get(&task.id).unwrap();
        assert_eq!(fetched, task);
    }

    #[test]
    fn test_get_not_found() {
        let (repo, _tmp) = repo();
        let err = repo.get("nonexistent").unwrap_err();
        assert!(matches!(err, TaskRepositoryError::NotFound(_)));
    }

    #[test]
    fn test_update_status() {
        let (repo, _tmp) = repo();
        let task = Task::new("Test", "/workspace", TaskMode::Craft);
        repo.create(&task).unwrap();
        repo.update_status(&task.id, TaskStatus::InProgress).unwrap();
        let fetched = repo.get(&task.id).unwrap();
        assert_eq!(fetched.status, TaskStatus::InProgress);
        assert!(fetched.updated_at >= task.updated_at);
    }

    #[test]
    fn test_update() {
        let (repo, _tmp) = repo();
        let mut task = Task::new("Original", "/workspace", TaskMode::Craft);
        repo.create(&task).unwrap();
        task.title = "Updated".to_string();
        task.status = TaskStatus::InProgress;
        repo.update(&task).unwrap();
        let fetched = repo.get(&task.id).unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(fetched.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_delete() {
        let (repo, _tmp) = repo();
        let task = Task::new("Test", "/workspace", TaskMode::Craft);
        repo.create(&task).unwrap();
        repo.delete(&task.id).unwrap();
        let err = repo.get(&task.id).unwrap_err();
        assert!(matches!(err, TaskRepositoryError::NotFound(_)));
    }

    #[test]
    fn test_list_all() {
        let (repo, _tmp) = repo();
        let t1 = Task::new("Task 1", "/ws", TaskMode::Craft);
        let t2 = Task::new("Task 2", "/ws", TaskMode::Plan);
        repo.create(&t1).unwrap();
        repo.create(&t2).unwrap();
        let tasks = repo.list(None, None).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    /// The `tasks` table is shared by every session of an install, so a
    /// scoped list must return **that session's** tasks plus the ones
    /// nobody has claimed yet — and never another session's.
    #[test]
    fn list_scopes_to_the_session_but_keeps_unclaimed_tasks_visible() {
        let (repo, _tmp) = repo();
        let mut mine = Task::new("Mine", "/ws", TaskMode::Craft);
        mine.session_id = Some("sess-1".to_string());
        let mut theirs = Task::new("Theirs", "/ws", TaskMode::Craft);
        theirs.session_id = Some("sess-2".to_string());
        let unclaimed = Task::new("Unclaimed", "/ws", TaskMode::Craft);

        repo.create(&mine).unwrap();
        repo.create(&theirs).unwrap();
        repo.create(&unclaimed).unwrap();

        let scoped = repo.list(None, Some("sess-1")).unwrap();
        assert!(
            scoped.iter().any(|t| t.id == mine.id),
            "本会话的任务必须在列表里"
        );
        assert!(
            scoped.iter().any(|t| t.id == unclaimed.id),
            "尚未归属的任务不能凭空消失"
        );
        assert!(
            !scoped.iter().any(|t| t.id == theirs.id),
            "别的会话的任务不能出现在本会话的列表里"
        );

        // No session → the global list (CLI / cron callers).
        assert_eq!(repo.list(None, None).unwrap().len(), 3);
    }

    #[test]
    fn test_list_by_status() {        let (repo, _tmp) = repo();
        let t1 = Task::new("Task 1", "/ws", TaskMode::Craft);
        let t2 = Task::new("Task 2", "/ws", TaskMode::Plan);
        repo.create(&t1).unwrap();
        repo.create(&t2).unwrap();
        repo.update_status(&t1.id, TaskStatus::Completed).unwrap();
        let completed = repo.list(Some(TaskStatus::Completed), None).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, t1.id);
        let pending = repo.list(Some(TaskStatus::Pending), None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, t2.id);
    }

    /// 老库升级：`CREATE TABLE IF NOT EXISTS` 不补列，而索引一旦引用尚不
    /// 存在的列，`execute_batch` 就在建索引处失败 —— 排在它后面的
    /// `ALTER TABLE` 永远不执行（调用方 `let _ = ensure_schema()` 还会把
    /// 错误丢掉），此后每条 `task_*` 查询都在运行期报 no such column。
    /// 这条测试先造一张没有 `session_id` 的旧表，再跑迁移。
    #[test]
    fn ensure_schema_migrates_a_legacy_tasks_table_without_the_session_column() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"CREATE TABLE tasks (
                    id              TEXT PRIMARY KEY,
                    title           TEXT NOT NULL,
                    workspace_path  TEXT NOT NULL,
                    mode            TEXT NOT NULL DEFAULT 'craft',
                    status          TEXT NOT NULL DEFAULT 'pending',
                    parent_task_id  TEXT,
                    context_json    TEXT NOT NULL DEFAULT '{}',
                    result_refs_json TEXT NOT NULL DEFAULT '[]',
                    created_at      INTEGER NOT NULL,
                    updated_at      INTEGER NOT NULL
                );
                INSERT INTO tasks (id, title, workspace_path, created_at, updated_at)
                VALUES ('old-1', 'legacy', '.', 1, 1);"#,
            )
            .unwrap();
        }

        let repo = TaskRepository::new(&path);
        repo.ensure_schema().expect("老库升级不能失败");

        // The legacy row survives, unclaimed, and stays readable.
        let scoped = repo.list(None, Some("sess-1")).unwrap();
        assert!(scoped.iter().any(|t| t.id == "old-1"), "老任务不能凭空消失");
        assert!(
            scoped.iter().all(|t| t.session_id.is_none()),
            "迁移出来的老行必须是无归属状态"
        );
        assert!(repo.get("old-1").is_ok(), "按下标取值的 SELECT 必须能跑");

        // Idempotent: duplicate column / duplicate index must not fire.
        repo.ensure_schema().unwrap();

        let conn = rusqlite::Connection::open(&path).unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='tasks'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            names.iter().any(|n| n == "idx_tasks_session"),
            "会话索引必须在补列之后建出来，实际：{names:?}"
        );
    }

    /// A `NULL`-owned row is claimable exactly once; a row already owned
    /// by another session must not change hands.
    #[test]
    fn claim_takes_over_only_unowned_tasks() {
        let (repo, _tmp) = repo();
        let unowned = Task::new("Orphan", "/ws", TaskMode::Craft);
        let mut theirs = Task::new("Theirs", "/ws", TaskMode::Craft);
        theirs.session_id = Some("sess-2".to_string());
        repo.create(&unowned).unwrap();
        repo.create(&theirs).unwrap();

        let claimed = repo.claim(&unowned.id, "sess-1").expect("未归属任务可被接手");
        assert_eq!(claimed.session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            repo.get(&unowned.id).unwrap().session_id.as_deref(),
            Some("sess-1"),
            "接手后必须落库"
        );

        // Already owned → nothing changes.
        assert!(repo.claim(&unowned.id, "sess-3").is_err());
        assert_eq!(
            repo.get(&unowned.id).unwrap().session_id.as_deref(),
            Some("sess-1")
        );
        // Another session's task is not claimable at all.
        assert!(repo.claim(&theirs.id, "sess-1").is_err());
        assert_eq!(
            repo.get(&theirs.id).unwrap().session_id.as_deref(),
            Some("sess-2")
        );
    }

    #[test]
    fn test_list_children() {
        let (repo, _tmp) = repo();
        let parent = Task::new("Parent", "/ws", TaskMode::Craft);
        let mut child = Task::new("Child", "/ws", TaskMode::Craft);
        child.parent_task_id = Some(parent.id.clone());
        repo.create(&parent).unwrap();
        repo.create(&child).unwrap();
        let children = repo.list_children(&parent.id).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
    }

    #[test]
    fn test_context_persistence() {
        let (repo, _tmp) = repo();
        let mut task = Task::new("Test", "/ws", TaskMode::Craft);
        task.context.notes = Some("Important note".to_string());
        task.context.screenshots = vec!["shot.png".to_string()];
        repo.create(&task).unwrap();
        let fetched = repo.get(&task.id).unwrap();
        assert_eq!(fetched.context.notes.as_deref(), Some("Important note"));
        assert_eq!(fetched.context.screenshots, vec!["shot.png".to_string()]);
    }

    #[test]
    fn test_results_persistence() {
        let (repo, _tmp) = repo();
        let mut task = Task::new("Test", "/ws", TaskMode::Craft);
        task.results = vec![TaskResultRef {
            path: "/output/report.html".to_string(),
            artifact_type: hermix_core::TaskArtifactType::Html,
            summary: Some("Generated".to_string()),
        }];
        repo.create(&task).unwrap();
        let fetched = repo.get(&task.id).unwrap();
        assert_eq!(fetched.results.len(), 1);
        assert_eq!(fetched.results[0].path, "/output/report.html");
    }
}
