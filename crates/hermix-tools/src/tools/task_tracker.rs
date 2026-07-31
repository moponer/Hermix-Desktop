//! Task tracker tools — `task_create` / `task_update` / `task_list`.
//!
//!
//!
//! These tools let the LLM manage orchestration-engine Tasks directly
//! from the conversation. The LLM can:
//! - `task_create` — create a new Task with a title and optional mode
//! - `task_update` — transition a Task's status (pending → in_progress → completed/failed)
//! - `task_list` — list Tasks with optional status filter
//!
//! The tools use a backend trait ([`TaskTrackerBackend`]) so the
//! concrete persistence layer (hermix-task's `TaskService`) can be
//! injected at registration time by the server. This avoids a
//! hermix-tools → hermix-task dependency.
//!
//! 铁律 T: When the LLM creates a task and completes it within the
//! same turn, the frontend panel may lag behind — this is a UI sync
//! issue, not a data integrity problem. The database is always correct.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema,
    traits::{PermissionMode, RiskLevel},
};

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// Backend for the task tracker tools. Implemented by the server using
/// `hermix_task::TaskService`.
#[async_trait]
pub trait TaskTrackerBackend: Send + Sync {
    /// Create a new Task.
    /// Returns the task ID.
    async fn create(
        &self,
        title: &str,
        workspace_path: Option<&str>,
        mode: Option<&str>,
        parent_task_id: Option<&str>,
    ) -> Result<TaskTrackerInfo, ToolError>;

    /// Update a Task's status.
    async fn update_status(
        &self,
        task_id: &str,
        status: &str,
    ) -> Result<TaskTrackerInfo, ToolError>;

    /// List Tasks, optionally filtered by status.
    async fn list(
        &self,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskTrackerInfo>, ToolError>;

    /// Get a single Task by ID.
    async fn get(&self, task_id: &str) -> Result<TaskTrackerInfo, ToolError>;

    /// List child tasks of a parent task.
    async fn list_children(
        &self,
        parent_task_id: &str,
    ) -> Result<Vec<TaskTrackerInfo>, ToolError>;

    /// Add a deliverable artifact to a Task .
    /// `path` is the absolute path or URL to the artifact.
    /// `artifact_type` is one of: html, image, report, video, file, directory.
    /// `summary` is an optional one-line description.
    async fn add_result(
        &self,
        task_id: &str,
        path: &str,
        artifact_type: &str,
        summary: Option<&str>,
    ) -> Result<TaskTrackerInfo, ToolError>;
}

/// Lightweight task info returned by the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTrackerInfo {
    pub id: String,
    pub title: String,
    pub status: String,
    pub mode: String,
    pub workspace_path: String,
    pub parent_task_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the three task tracker tools with the registry.
pub fn register(registry: &ToolRegistry, backend: Arc<dyn TaskTrackerBackend>) {
    // task_create
    let create_handler = Arc::new(TaskCreateHandler {
        backend: backend.clone(),
    });
    registry.register_with_permission(
        "task_create",
        "task",
        create_handler.schema(),
        create_handler,
        Arc::new(|| true),
        vec![],
        false,
        "Create a new task in the orchestration engine. Use this to track work items.",
        "📋",
        None,
        RiskLevel::Safe,
        PermissionMode::WorkspaceWrite,
        false,
    );

    // task_update
    let update_handler = Arc::new(TaskUpdateHandler {
        backend: backend.clone(),
    });
    registry.register_with_permission(
        "task_update",
        "task",
        update_handler.schema(),
        update_handler,
        Arc::new(|| true),
        vec![],
        false,
        "Update a task's status. Valid transitions: pending→in_progress→completed/failed.",
        "📋",
        None,
        RiskLevel::Safe,
        PermissionMode::WorkspaceWrite,
        false,
    );

    // task_list
    let list_handler = Arc::new(TaskListHandler {
        backend: backend.clone(),
    });
    registry.register_with_permission(
        "task_list",
        "task",
        list_handler.schema(),
        list_handler,
        Arc::new(|| true),
        vec![],
        false,
        "List tasks with optional status filter. Returns all tasks if no filter.",
        "📋",
        None,
        RiskLevel::Safe,
        PermissionMode::ReadOnly,
        false,
    );

    // task_get
    let get_handler = Arc::new(TaskGetHandler {
        backend: backend.clone(),
    });
    registry.register_with_permission(
        "task_get",
        "task",
        get_handler.schema(),
        get_handler,
        Arc::new(|| true),
        vec![],
        false,
        "Get a single task by ID, including its child tasks (subtasks).",
        "📋",
        None,
        RiskLevel::Safe,
        PermissionMode::ReadOnly,
        false,
    );

    //present_files
    let present_handler = Arc::new(PresentFilesHandler {
        backend: backend.clone(),
    });
    registry.register_with_permission(
        "present_files",
        "task",
        present_handler.schema(),
        present_handler,
        Arc::new(|| true),
        vec![],
        false,
        "Register deliverable artifacts for a task. Call this when you produce output files (HTML reports, images, documents, etc.) that should be shown to the user in the results panel.",
        "📦",
        None,
        RiskLevel::Safe,
        PermissionMode::WorkspaceWrite,
        false,
    );
}

// ---------------------------------------------------------------------------
// TaskCreateHandler
// ---------------------------------------------------------------------------

struct TaskCreateHandler {
    backend: Arc<dyn TaskTrackerBackend>,
}

#[async_trait]
impl ToolHandler for TaskCreateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'title' parameter".into()))?;

        let workspace_path = params.get("workspace_path").and_then(|v| v.as_str());
        let mode = params.get("mode").and_then(|v| v.as_str());
        let parent_task_id = params.get("parent_task_id").and_then(|v| v.as_str());

        let task = self
            .backend
            .create(title, workspace_path, mode, parent_task_id)
            .await?;

        Ok(json!({
            "status": "created",
            "task_id": task.id,
            "title": task.title,
            "mode": task.mode,
            "message": format!(
                "Task '{}' created with mode '{}'. Use task_update(task_id='{}', status='in_progress') to start working on it.",
                task.title, task.mode, task.id
            )
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Human-readable task title"
            }),
        );
        props.insert(
            "workspace_path".into(),
            json!({
                "type": "string",
                "description": "Workspace directory path (optional, defaults to current workspace)"
            }),
        );
        props.insert(
            "mode".into(),
            json!({
                "type": "string",
                "description": "Working mode: craft (direct execution), plan (plan-then-execute), ask (read-only)",
                "enum": ["craft", "plan", "ask"],
                "default": "craft"
            }),
        );
        props.insert(
            "parent_task_id".into(),
            json!({
                "type": "string",
                "description": "Parent task ID for subtask creation (optional)"
            }),
        );

        tool_schema(
            "task_create",
            "Create a new task to track a unit of work.",
            JsonSchema::object(props, vec!["title".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// TaskUpdateHandler
// ---------------------------------------------------------------------------

struct TaskUpdateHandler {
    backend: Arc<dyn TaskTrackerBackend>,
}

#[async_trait]
impl ToolHandler for TaskUpdateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;

        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'status' parameter".into()))?;

        let task = self.backend.update_status(task_id, status).await?;

        Ok(json!({
            "status": "updated",
            "task_id": task.id,
            "new_status": task.status,
            "title": task.title,
            "message": format!("Task '{}' is now '{}'.", task.title, task.status)
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "task_id".into(),
            json!({
                "type": "string",
                "description": "The task ID to update"
            }),
        );
        props.insert(
            "status".into(),
            json!({
                "type": "string",
                "description": "New status. Valid transitions: pending→planning, pending→in_progress, planning→in_progress, in_progress→completed, in_progress→failed",
                "enum": ["pending", "planning", "in_progress", "completed", "failed", "archived"]
            }),
        );

        tool_schema(
            "task_update",
            "Update a task's status. Follows the state machine: pending → planning → in_progress → completed/failed.",
            JsonSchema::object(props, vec!["task_id".into(), "status".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// TaskListHandler
// ---------------------------------------------------------------------------

struct TaskListHandler {
    backend: Arc<dyn TaskTrackerBackend>,
}

#[async_trait]
impl ToolHandler for TaskListHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let status_filter = params.get("status").and_then(|v| v.as_str());

        let tasks = self.backend.list(status_filter).await?;

        Ok(json!({
            "tasks": tasks,
            "total": tasks.len()
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "status".into(),
            json!({
                "type": "string",
                "description": "Filter by status (optional). If omitted, returns all tasks.",
                "enum": ["pending", "planning", "in_progress", "completed", "failed", "archived"]
            }),
        );

        tool_schema(
            "task_list",
            "List tasks with optional status filter.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// TaskGetHandler
// ---------------------------------------------------------------------------

struct TaskGetHandler {
    backend: Arc<dyn TaskTrackerBackend>,
}

#[async_trait]
impl ToolHandler for TaskGetHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;

        let task = self.backend.get(task_id).await?;
        let children = self.backend.list_children(task_id).await?;

        Ok(json!({
            "task": task,
            "children": children,
            "children_count": children.len(),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "task_id".into(),
            json!({
                "type": "string",
                "description": "The task ID to retrieve"
            }),
        );

        tool_schema(
            "task_get",
            "Get a single task by ID, including its child tasks (subtasks).",
            JsonSchema::object(props, vec!["task_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
//PresentFilesHandler
// ---------------------------------------------------------------------------

struct PresentFilesHandler {
    backend: Arc<dyn TaskTrackerBackend>,
}

#[async_trait]
impl ToolHandler for PresentFilesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;

        let files = params
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'files' array parameter".into()))?;

        if files.is_empty() {
            return Err(ToolError::InvalidParams(
                "'files' array must not be empty".into(),
            ));
        }

        let _summary = params.get("summary").and_then(|v| v.as_str());
        let mut delivered = Vec::new();
        let mut errors = Vec::new();

        for file in files {
            let path = file
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::InvalidParams("Each file must have a 'path' field".into())
                })?;

            let artifact_type = file
                .get("artifact_type")
                .and_then(|v| v.as_str())
                .unwrap_or("file");

            let file_summary = file.get("summary").and_then(|v| v.as_str());

            match self
                .backend
                .add_result(task_id, path, artifact_type, file_summary)
                .await
            {
                Ok(_) => {
                    delivered.push(json!({
                        "path": path,
                        "artifact_type": artifact_type,
                        "status": "delivered"
                    }));
                }
                Err(e) => {
                    errors.push(json!({
                        "path": path,
                        "error": e.to_string()
                    }));
                }
            }
        }

        Ok(json!({
            "task_id": task_id,
            "delivered_count": delivered.len(),
            "error_count": errors.len(),
            "delivered": delivered,
            "errors": errors,
            "message": format!(
                "Delivered {} file(s) to task '{}'.",
                delivered.len(),
                task_id
            )
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "task_id".into(),
            json!({
                "type": "string",
                "description": "The task ID to deliver artifacts to"
            }),
        );
        props.insert(
            "files".into(),
            json!({
                "type": "array",
                "description": "List of file paths or URLs to deliver",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute file path or URL"
                        },
                        "artifact_type": {
                            "type": "string",
                            "enum": ["html", "image", "report", "video", "file", "directory"],
                            "description": "Artifact type for preview rendering. Auto-detected from extension if omitted."
                        },
                        "summary": {
                            "type": "string",
                            "description": "Optional one-line summary of the artifact"
                        }
                    },
                    "required": ["path"]
                }
            }),
        );
        props.insert(
            "summary".into(),
            json!({
                "type": "string",
                "description": "Optional overall summary for this delivery"
            }),
        );

        tool_schema(
            "present_files",
            "Register deliverable artifacts for a task. Call this when you produce output files (HTML reports, images, documents, etc.) that should be shown to the user in the results panel.",
            JsonSchema::object(props, vec!["task_id".into(), "files".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory test backend.
    struct TestBackend {
        tasks: Mutex<Vec<TaskTrackerInfo>>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                tasks: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TaskTrackerBackend for TestBackend {
        async fn create(
            &self,
            title: &str,
            workspace_path: Option<&str>,
            mode: Option<&str>,
            parent_task_id: Option<&str>,
        ) -> Result<TaskTrackerInfo, ToolError> {
            let now = chrono::Utc::now().timestamp_millis();
            let task = TaskTrackerInfo {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.to_string(),
                status: "pending".to_string(),
                mode: mode.unwrap_or("craft").to_string(),
                workspace_path: workspace_path.unwrap_or(".").to_string(),
                parent_task_id: parent_task_id.map(String::from),
                created_at: now,
                updated_at: now,
            };
            let mut tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            tasks.push(task.clone());
            Ok(task)
        }

        async fn update_status(
            &self,
            task_id: &str,
            status: &str,
        ) -> Result<TaskTrackerInfo, ToolError> {
            let mut tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            let task = tasks
                .iter_mut()
                .find(|t| t.id == task_id)
                .ok_or_else(|| {
                    ToolError::NotFound(format!("Task '{}' not found", task_id))
                })?;
            task.status = status.to_string();
            task.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(task.clone())
        }

        async fn list(
            &self,
            status_filter: Option<&str>,
        ) -> Result<Vec<TaskTrackerInfo>, ToolError> {
            let tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            let filtered: Vec<TaskTrackerInfo> = match status_filter {
                Some(s) => tasks.iter().filter(|t| t.status == s).cloned().collect(),
                None => tasks.iter().cloned().collect(),
            };
            Ok(filtered)
        }

        async fn get(&self, task_id: &str) -> Result<TaskTrackerInfo, ToolError> {
            let tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            tasks
                .iter()
                .find(|t| t.id == task_id)
                .cloned()
                .ok_or_else(|| {
                    ToolError::NotFound(format!("Task '{}' not found", task_id))
                })
        }

        async fn list_children(
            &self,
            parent_task_id: &str,
        ) -> Result<Vec<TaskTrackerInfo>, ToolError> {
            let tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            Ok(tasks
                .iter()
                .filter(|t| t.parent_task_id.as_deref() == Some(parent_task_id))
                .cloned()
                .collect())
        }

        async fn add_result(
            &self,
            task_id: &str,
            path: &str,
            artifact_type: &str,
            summary: Option<&str>,
        ) -> Result<TaskTrackerInfo, ToolError> {
            let mut tasks = self.tasks.lock().map_err(|e| {
                ToolError::ExecutionFailed(e.to_string())
            })?;
            let task = tasks
                .iter_mut()
                .find(|t| t.id == task_id)
                .ok_or_else(|| {
                    ToolError::NotFound(format!("Task '{}' not found", task_id))
                })?;
            task.updated_at = chrono::Utc::now().timestamp_millis();
            // In the test backend, we just return the task — the real
            // backend persists the result ref.
            let _ = (path, artifact_type, summary);
            Ok(task.clone())
        }
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let backend = Arc::new(TestBackend::new());
        let task = backend
            .create("Test Task", Some("/ws"), Some("craft"), None)
            .await
            .unwrap();
        assert_eq!(task.title, "Test Task");
        assert_eq!(task.status, "pending");

        let tasks = backend.list(None).await.unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[tokio::test]
    async fn test_update_status() {
        let backend = Arc::new(TestBackend::new());
        let task = backend
            .create("Task", None, None, None)
            .await
            .unwrap();
        let updated = backend
            .update_status(&task.id, "in_progress")
            .await
            .unwrap();
        assert_eq!(updated.status, "in_progress");
    }

    #[tokio::test]
    async fn test_list_with_filter() {
        let backend = Arc::new(TestBackend::new());
        backend.create("T1", None, None, None).await.unwrap();
        let t2 = backend.create("T2", None, None, None).await.unwrap();
        backend.update_status(&t2.id, "completed").await.unwrap();

        let pending = backend.list(Some("pending")).await.unwrap();
        assert_eq!(pending.len(), 1);
        let completed = backend.list(Some("completed")).await.unwrap();
        assert_eq!(completed.len(), 1);
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let backend = Arc::new(TestBackend::new());
        let result = backend.get("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_subtask_with_parent() {
        let backend = Arc::new(TestBackend::new());
        let parent = backend
            .create("Parent Task", Some("/ws"), Some("craft"), None)
            .await
            .unwrap();
        let child = backend
            .create("Child Task", Some("/ws"), Some("craft"), Some(&parent.id))
            .await
            .unwrap();
        assert_eq!(child.parent_task_id.as_deref(), Some(parent.id.as_str()));
    }

    #[tokio::test]
    async fn test_list_children() {
        let backend = Arc::new(TestBackend::new());
        let parent = backend
            .create("Parent", None, None, None)
            .await
            .unwrap();
        backend
            .create("Child 1", None, None, Some(&parent.id))
            .await
            .unwrap();
        backend
            .create("Child 2", None, None, Some(&parent.id))
            .await
            .unwrap();
        backend
            .create("Unrelated", None, None, None)
            .await
            .unwrap();

        let children = backend.list_children(&parent.id).await.unwrap();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|c| c.parent_task_id.as_deref() == Some(parent.id.as_str())));
    }

    #[tokio::test]
    async fn test_same_turn_create_and_complete() {
        // 铁律 T: creating and completing a task in the same turn is valid.
        // The frontend panel may lag behind, but the database is always correct.
        let backend = Arc::new(TestBackend::new());
        let task = backend
            .create("Quick Task", None, Some("craft"), None)
            .await
            .unwrap();
        let in_progress = backend
            .update_status(&task.id, "in_progress")
            .await
            .unwrap();
        assert_eq!(in_progress.status, "in_progress");
        let completed = backend
            .update_status(&task.id, "completed")
            .await
            .unwrap();
        assert_eq!(completed.status, "completed");
    }

    #[tokio::test]
    async fn test_get_returns_children() {
        let backend = Arc::new(TestBackend::new());
        let parent = backend
            .create("Parent", None, None, None)
            .await
            .unwrap();
        backend
            .create("Child 1", None, None, Some(&parent.id))
            .await
            .unwrap();
        backend
            .create("Child 2", None, None, Some(&parent.id))
            .await
            .unwrap();

        let task = backend.get(&parent.id).await.unwrap();
        assert_eq!(task.id, parent.id);

        let children = backend.list_children(&parent.id).await.unwrap();
        assert_eq!(children.len(), 2);
    }

    #[tokio::test]
    async fn test_decompose_large_task() {
        //Simulate LLM decomposing a large task into subtasks
        let backend = Arc::new(TestBackend::new());

        // 1. Create parent task
        let parent = backend
            .create("Implement Feature X", Some("/ws"), Some("craft"), None)
            .await
            .unwrap();

        // 2. Create subtasks
        let sub1 = backend
            .create("Step 1: Research", Some("/ws"), None, Some(&parent.id))
            .await
            .unwrap();
        let sub2 = backend
            .create("Step 2: Implement", Some("/ws"), None, Some(&parent.id))
            .await
            .unwrap();
        let sub3 = backend
            .create("Step 3: Test", Some("/ws"), None, Some(&parent.id))
            .await
            .unwrap();

        // 3. Start and complete subtasks
        backend.update_status(&sub1.id, "in_progress").await.unwrap();
        backend.update_status(&sub1.id, "completed").await.unwrap();
        backend.update_status(&sub2.id, "in_progress").await.unwrap();
        backend.update_status(&sub2.id, "completed").await.unwrap();
        backend.update_status(&sub3.id, "in_progress").await.unwrap();
        backend.update_status(&sub3.id, "completed").await.unwrap();

        // 4. Complete parent
        backend.update_status(&parent.id, "in_progress").await.unwrap();
        backend.update_status(&parent.id, "completed").await.unwrap();

        // 5. Verify
        let children = backend.list_children(&parent.id).await.unwrap();
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|c| c.status == "completed"));

        let all_completed = backend.list(Some("completed")).await.unwrap();
        assert_eq!(all_completed.len(), 4); // parent + 3 subtasks
    }
}
