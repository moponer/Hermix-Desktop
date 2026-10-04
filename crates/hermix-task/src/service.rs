//! Task lifecycle service — create/query/update/archive with
//! state-machine validation.
//!
//! Wraps [`TaskRepository`] with business logic:
//! - Mode-aware status transitions (Plan → planning → in_progress)
//! - Emit orchestration events on lifecycle changes
//! - Context and results management
//!
//!

use std::sync::Arc;

use hermix_core::{TaskContext, TaskMode, TaskResultRef, TaskStatus};

use crate::{Task, TaskRepository, TaskRepositoryError};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TaskServiceError {
    #[error("Repository error: {0}")]
    Repository(#[from] TaskRepositoryError),
    #[error("Invalid transition: {0}")]
    InvalidTransition(String),
    #[error("Task not found: {0}")]
    NotFound(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

// ---------------------------------------------------------------------------
// Create params
// ---------------------------------------------------------------------------

/// Parameters for creating a new Task.
#[derive(Debug, Clone)]
pub struct CreateTaskParams {
    pub title: String,
    pub workspace_path: String,
    pub mode: TaskMode,
    pub parent_task_id: Option<String>,
    pub context: TaskContext,
}

impl Default for CreateTaskParams {
    fn default() -> Self {
        Self {
            title: "Untitled Task".to_string(),
            workspace_path: ".".to_string(),
            mode: TaskMode::Craft,
            parent_task_id: None,
            context: TaskContext::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskService
// ---------------------------------------------------------------------------

/// Lifecycle management service for Tasks.
/// Stateless except for the repository reference — all state is in
/// the database. Thread-safe via `Arc<TaskRepository>`.
pub struct TaskService {
    repo: Arc<TaskRepository>,
}

impl TaskService {
    /// Create a new service backed by the given repository.
    pub fn new(repo: Arc<TaskRepository>) -> Self {
        Self { repo }
    }

    /// Create a new Task.
    /// For `Plan` mode, the task starts in `Pending` and transitions
    /// to `Planning` when the agent begins planning. For `Craft`, the
    /// task starts in `Pending` and transitions directly to
    /// `InProgress` when execution begins. For `Ask`, the task stays
    /// in `Pending` (answered, not executed).
    pub fn create(&self, params: CreateTaskParams) -> Result<Task, TaskServiceError> {
        if params.title.trim().is_empty() {
            return Err(TaskServiceError::Validation("title cannot be empty".into()));
        }
        if params.workspace_path.trim().is_empty() {
            return Err(TaskServiceError::Validation(
                "workspace_path cannot be empty".into(),
            ));
        }
        let mut task = Task::new(params.title, params.workspace_path, params.mode);
        task.parent_task_id = params.parent_task_id;
        task.context = params.context;
        self.repo.create(&task)?;
        Ok(task)
    }

    /// Get a Task by ID.
    pub fn get(&self, id: &str) -> Result<Task, TaskServiceError> {
        self.repo.get(id).map_err(|e| match e {
            TaskRepositoryError::NotFound(id) => TaskServiceError::NotFound(id),
            other => TaskServiceError::Repository(other),
        })
    }

    /// Transition a Task to a new status.
    /// Validates the transition via [`TaskStatus::validate_transition`].
    /// For `Plan` mode, `pending → planning` is the expected first
    /// transition; for `Craft`, `pending → in_progress` is expected.
    /// Returns the updated Task.
    pub fn transition(
        &self,
        id: &str,
        new_status: TaskStatus,
    ) -> Result<Task, TaskServiceError> {
        let task = self.get(id)?;
        task.status
            .validate_transition(new_status)
            .map_err(TaskServiceError::InvalidTransition)?;

        // Mode-specific rules:
        // - Ask mode cannot enter InProgress or Planning
        if task.mode == TaskMode::Ask
            && matches!(new_status, TaskStatus::InProgress | TaskStatus::Planning)
        {
            return Err(TaskServiceError::InvalidTransition(
                "Ask mode tasks cannot enter execution states".into(),
            ));
        }

        self.repo.update_status(id, new_status)?;
        self.get(id)
    }

    /// Update a Task's title.
    pub fn update_title(&self, id: &str, title: &str) -> Result<Task, TaskServiceError> {
        if title.trim().is_empty() {
            return Err(TaskServiceError::Validation("title cannot be empty".into()));
        }
        let mut task = self.get(id)?;
        task.title = title.to_string();
        self.repo.update(&task)?;
        self.get(id)
    }

    /// Update a Task's context.
    pub fn update_context(
        &self,
        id: &str,
        context: TaskContext,
    ) -> Result<Task, TaskServiceError> {
        let mut task = self.get(id)?;
        task.context = context;
        self.repo.update(&task)?;
        self.get(id)
    }

    /// Add a result reference to a Task.
    pub fn add_result(
        &self,
        id: &str,
        result: TaskResultRef,
    ) -> Result<Task, TaskServiceError> {
        let mut task = self.get(id)?;
        task.results.push(result);
        self.repo.update(&task)?;
        self.get(id)
    }

    /// Archive a Task. This is a terminal transition — the task
    /// cannot be reactivated after archiving.
    pub fn archive(&self, id: &str) -> Result<Task, TaskServiceError> {
        self.transition(id, TaskStatus::Archived)
    }

    /// List all tasks, optionally filtered by status.
    pub fn list(&self, status_filter: Option<TaskStatus>) -> Result<Vec<Task>, TaskServiceError> {
        self.repo.list(status_filter).map_err(Into::into)
    }

    /// List child tasks of a parent.
    pub fn list_children(&self, parent_task_id: &str) -> Result<Vec<Task>, TaskServiceError> {
        self.repo
            .list_children(parent_task_id)
            .map_err(Into::into)
    }

    /// Convenience: start a Plan-mode task (pending → planning).
    pub fn start_planning(&self, id: &str) -> Result<Task, TaskServiceError> {
        self.transition(id, TaskStatus::Planning)
    }

    /// Convenience: start execution (planning/pending → in_progress).
    pub fn start_execution(&self, id: &str) -> Result<Task, TaskServiceError> {
        let task = self.get(id)?;
        // For Plan mode, must go through planning first
        if task.mode == TaskMode::Plan && task.status == TaskStatus::Pending {
            return Err(TaskServiceError::InvalidTransition(
                "Plan mode tasks must transition through Planning first".into(),
            ));
        }
        self.transition(id, TaskStatus::InProgress)
    }

    /// Convenience: mark a task as completed.
    pub fn complete(&self, id: &str) -> Result<Task, TaskServiceError> {
        self.transition(id, TaskStatus::Completed)
    }

    /// Convenience: mark a task as failed.
    ///
    /// `Pending` is accepted because a caller can fail a task it created
    /// moments ago without ever driving it through `Planning` /
    /// `InProgress` — automations are the case in point: one creates a
    /// task and immediately runs the agent in the same call, so the task
    /// is still `Pending` when the run comes back failed. Without this,
    /// `fail` rejected it, the caller logged a warning and moved on, and
    /// the task sat in `Pending` forever with nothing in the UI to say the
    /// run had failed.
    pub fn fail(&self, id: &str) -> Result<Task, TaskServiceError> {
        let task = self.get(id)?;
        if !matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::Planning | TaskStatus::InProgress
        ) {
            return Err(TaskServiceError::InvalidTransition(format!(
                "Cannot fail from {:?}",
                task.status
            )));
        }
        self.transition(id, TaskStatus::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (TaskService, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(TaskRepository::new(tmp.path().join("test.db")));
        repo.ensure_schema().unwrap();
        let svc = TaskService::new(repo);
        (svc, tmp)
    }

    #[test]
    fn test_create_task() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "My Task".into(),
                workspace_path: "/ws".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(task.title, "My Task");
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[test]
    fn test_create_empty_title_rejected() {
        let (svc, _tmp) = service();
        let err = svc
            .create(CreateTaskParams {
                title: "  ".into(),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, TaskServiceError::Validation(_)));
    }

    #[test]
    fn test_craft_transition_pending_to_in_progress() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Craft".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let updated = svc.start_execution(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_plan_must_go_through_planning() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Plan".into(),
                mode: TaskMode::Plan,
                ..Default::default()
            })
            .unwrap();
        // Direct pending → in_progress should fail for Plan mode
        let err = svc.start_execution(&task.id).unwrap_err();
        assert!(matches!(err, TaskServiceError::InvalidTransition(_)));
        // Must go through planning first
        svc.start_planning(&task.id).unwrap();
        let updated = svc.start_execution(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_ask_cannot_enter_in_progress() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Ask".into(),
                mode: TaskMode::Ask,
                ..Default::default()
            })
            .unwrap();
        let err = svc.transition(&task.id, TaskStatus::InProgress).unwrap_err();
        assert!(matches!(err, TaskServiceError::InvalidTransition(_)));
    }

    #[test]
    fn test_complete_task() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        svc.start_execution(&task.id).unwrap();
        let completed = svc.complete(&task.id).unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(completed.is_terminal());
    }

    #[test]
    fn test_fail_task() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        svc.start_execution(&task.id).unwrap();
        let failed = svc.fail(&task.id).unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
    }

    /// `Pending` 可以直接 fail——创建后立刻失败的场景是真实存在的。
    ///
    /// 本测试原先断言相反（"pending task 不能 fail"），那条规则在自动化
    /// 路径上产生了静默失败：自动化建 Task 后立即跑 agent，run 失败时
    /// Task 还是 `Pending`，`fail` 被拒、调用方只 `warn!` 一下，Task 就永久
    /// 停在 `Pending`——UI 上看不出这次运行失败了。改为允许，并让
    /// `validate_transition` 同步认可 `Pending → Failed`，避免
    /// `fail()` 与 `transition()` 两套判定分叉。
    #[test]
    fn test_can_fail_pending_task() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let failed = svc.fail(&task.id).unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
    }

    /// 终态不可再回退到 Failed。
    #[test]
    fn test_cannot_fail_finished_task() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        svc.complete(&task.id).unwrap();
        let err = svc.fail(&task.id).unwrap_err();
        assert!(matches!(err, TaskServiceError::InvalidTransition(_)));
    }

    #[test]
    fn test_archive() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let archived = svc.archive(&task.id).unwrap();
        assert_eq!(archived.status, TaskStatus::Archived);
    }

    #[test]
    fn test_add_result() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let updated = svc
            .add_result(
                &task.id,
                TaskResultRef {
                    path: "/output.html".into(),
                    artifact_type: hermix_core::TaskArtifactType::Html,
                    summary: None,
                },
            )
            .unwrap();
        assert_eq!(updated.results.len(), 1);
    }

    #[test]
    fn test_update_context() {
        let (svc, _tmp) = service();
        let task = svc
            .create(CreateTaskParams {
                title: "Task".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let mut ctx = TaskContext::default();
        ctx.notes = Some("New notes".into());
        let updated = svc.update_context(&task.id, ctx).unwrap();
        assert_eq!(updated.context.notes.as_deref(), Some("New notes"));
    }

    #[test]
    fn test_list_by_status() {
        let (svc, _tmp) = service();
        let t1 = svc
            .create(CreateTaskParams {
                title: "T1".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        let _t2 = svc
            .create(CreateTaskParams {
                title: "T2".into(),
                mode: TaskMode::Craft,
                ..Default::default()
            })
            .unwrap();
        svc.start_execution(&t1.id).unwrap();
        let in_progress = svc.list(Some(TaskStatus::InProgress)).unwrap();
        assert_eq!(in_progress.len(), 1);
        let pending = svc.list(Some(TaskStatus::Pending)).unwrap();
        assert_eq!(pending.len(), 1);
    }
}
