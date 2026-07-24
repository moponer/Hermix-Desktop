//! SQLite-backed CRUD for the `tasks` table.
//!
//! Shares the same `sessions.db` file as `SessionPersistence` (铁律 D/WAL).
//! All operations go through [`open_with_wal`] to ensure WAL mode +
//! busy_timeout are set on every connection.
//!
//! (Phase 1 — P1-1)

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
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| TaskRepositoryError::Io(format!("Failed to open tasks db: {e}")))?;
    if let Err(e) =
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
    {
        tracing::warn!(
            "task_repository: WAL/busy_timeout PRAGMA failed ({e}); continuing"
        );
    }
    // Enable foreign key cascade deletes for automation tables.
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
    Ok(conn)
}

// ---------------------------------------------------------------------------
// TaskRepository
// ---------------------------------------------------------------------------

/// SQLite-backed repository for `Task` entities.
/// /// Stateless — each method opens a fresh connection to the DB. This
/// matches the pattern used by `SessionPersistence` and avoids
/// connection lifecycle issues in multi-threaded contexts.
pub struct TaskRepository {
    db_path: PathBuf,
}

impl TaskRepository {
    /// Create a new repository pointing at the given SQLite database.
    /// /// In production, pass `hermix_config::paths::tasks_db_path()`.
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
                updated_at      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);"#,
        )?;
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
                context_json, result_refs_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                        context_json, result_refs_json, created_at, updated_at
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
    pub fn list(
        &self,
        status_filter: Option<TaskStatus>,
    ) -> Result<Vec<Task>, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut tasks = Vec::new();
        if let Some(status) = status_filter {
            let status_str = match status {
                TaskStatus::Pending => "pending",
                TaskStatus::Planning => "planning",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Completed => "completed",
                TaskStatus::Failed => "failed",
                TaskStatus::Archived => "archived",
            };
            let mut stmt = conn.prepare(
                "SELECT id, title, workspace_path, mode, status, parent_task_id,
                        context_json, result_refs_json, created_at, updated_at
                 FROM tasks WHERE status = ?1
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([status_str], row_to_task)?;
            for row in rows {
                tasks.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, title, workspace_path, mode, status, parent_task_id,
                        context_json, result_refs_json, created_at, updated_at
                 FROM tasks
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_task)?;
            for row in rows {
                tasks.push(row?);
            }
        }
        Ok(tasks)
    }

    /// List child tasks of a parent task.
    pub fn list_children(&self, parent_task_id: &str) -> Result<Vec<Task>, TaskRepositoryError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, title, workspace_path, mode, status, parent_task_id,
                    context_json, result_refs_json, created_at, updated_at
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
        let tasks = repo.list(None).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_list_by_status() {
        let (repo, _tmp) = repo();
        let t1 = Task::new("Task 1", "/ws", TaskMode::Craft);
        let t2 = Task::new("Task 2", "/ws", TaskMode::Plan);
        repo.create(&t1).unwrap();
        repo.create(&t2).unwrap();
        repo.update_status(&t1.id, TaskStatus::Completed).unwrap();
        let completed = repo.list(Some(TaskStatus::Completed)).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, t1.id);
        let pending = repo.list(Some(TaskStatus::Pending)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, t2.id);
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
