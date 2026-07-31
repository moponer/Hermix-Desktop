//! Adapter that implements `hermix_tools::tools::task_tracker::TaskTrackerBackend`
//! using `hermix_task::TaskService`.
//!
//!
//!
//! This bridges the tool layer (hermix-tools) to the domain layer
//! (hermix-task) without creating a direct dependency between them.

use std::sync::Arc;

use async_trait::async_trait;
use hermix_core::TaskMode;
use hermix_tools::tools::task_tracker::{TaskTrackerBackend, TaskTrackerInfo};
use hermix_task::{TaskService, TaskServiceError};
use hermix_task::service::CreateTaskParams;

/// Adapter wrapping `TaskService` to implement `TaskTrackerBackend`.
pub struct TaskTrackerBackendImpl {
    service: Arc<TaskService>,
}

impl TaskTrackerBackendImpl {
    pub fn new(service: Arc<TaskService>) -> Self {
        Self { service }
    }
}

fn task_to_info(task: &hermix_task::Task) -> TaskTrackerInfo {
    TaskTrackerInfo {
        id: task.id.clone(),
        title: task.title.clone(),
        status: format!("{:?}", task.status).to_lowercase(),
        mode: format!("{:?}", task.mode).to_lowercase(),
        workspace_path: task.workspace_path.clone(),
        parent_task_id: task.parent_task_id.clone(),
        created_at: task.created_at,
        updated_at: task.updated_at,
    }
}

fn parse_status(s: &str) -> Result<hermix_core::TaskStatus, String> {
    match s.to_lowercase().as_str() {
        "pending" => Ok(hermix_core::TaskStatus::Pending),
        "planning" => Ok(hermix_core::TaskStatus::Planning),
        "in_progress" => Ok(hermix_core::TaskStatus::InProgress),
        "completed" => Ok(hermix_core::TaskStatus::Completed),
        "failed" => Ok(hermix_core::TaskStatus::Failed),
        "archived" => Ok(hermix_core::TaskStatus::Archived),
        other => Err(format!("Unknown status: {}", other)),
    }
}

fn parse_mode(s: Option<&str>) -> TaskMode {
    match s.map(|m| m.to_lowercase()).as_deref() {
        Some("plan") => TaskMode::Plan,
        Some("ask") => TaskMode::Ask,
        _ => TaskMode::Craft,
    }
}

fn map_error(e: TaskServiceError) -> hermix_core::ToolError {
    use hermix_core::ToolError;
    match e {
        TaskServiceError::NotFound(id) => ToolError::NotFound(format!("Task '{}' not found", id)),
        TaskServiceError::InvalidTransition(msg) => {
            ToolError::InvalidParams(format!("Invalid status transition: {}", msg))
        }
        TaskServiceError::Validation(msg) => ToolError::InvalidParams(msg),
        TaskServiceError::Repository(e) => {
            ToolError::ExecutionFailed(format!("Repository error: {}", e))
        }
    }
}

#[async_trait]
impl TaskTrackerBackend for TaskTrackerBackendImpl {
    async fn create(
        &self,
        title: &str,
        workspace_path: Option<&str>,
        mode: Option<&str>,
        parent_task_id: Option<&str>,
    ) -> Result<TaskTrackerInfo, hermix_core::ToolError> {
        let params = CreateTaskParams {
            title: title.to_string(),
            workspace_path: workspace_path.unwrap_or(".").to_string(),
            mode: parse_mode(mode),
            parent_task_id: parent_task_id.map(String::from),
            context: hermix_core::TaskContext::default(),
        };
        let task = self.service.create(params).map_err(map_error)?;
        Ok(task_to_info(&task))
    }

    async fn update_status(
        &self,
        task_id: &str,
        status: &str,
    ) -> Result<TaskTrackerInfo, hermix_core::ToolError> {
        let new_status = parse_status(status).map_err(hermix_core::ToolError::InvalidParams)?;
        let task = self
            .service
            .transition(task_id, new_status)
            .map_err(map_error)?;
        Ok(task_to_info(&task))
    }

    async fn list(
        &self,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskTrackerInfo>, hermix_core::ToolError> {
        let filter = match status_filter {
            Some(s) => Some(parse_status(s).map_err(hermix_core::ToolError::InvalidParams)?),
            None => None,
        };
        let tasks = self.service.list(filter).map_err(map_error)?;
        Ok(tasks.iter().map(task_to_info).collect())
    }

    async fn get(&self, task_id: &str) -> Result<TaskTrackerInfo, hermix_core::ToolError> {
        let task = self.service.get(task_id).map_err(map_error)?;
        Ok(task_to_info(&task))
    }

    async fn list_children(
        &self,
        parent_task_id: &str,
    ) -> Result<Vec<TaskTrackerInfo>, hermix_core::ToolError> {
        let tasks = self
            .service
            .list_children(parent_task_id)
            .map_err(map_error)?;
        Ok(tasks.iter().map(task_to_info).collect())
    }

    async fn add_result(
        &self,
        task_id: &str,
        path: &str,
        artifact_type: &str,
        summary: Option<&str>,
    ) -> Result<TaskTrackerInfo, hermix_core::ToolError> {
        let artifact_type = match artifact_type.to_lowercase().as_str() {
            "html" => hermix_core::TaskArtifactType::Html,
            "image" => hermix_core::TaskArtifactType::Image,
            "report" => hermix_core::TaskArtifactType::Report,
            "video" => hermix_core::TaskArtifactType::Video,
            "directory" => hermix_core::TaskArtifactType::Directory,
            _ => hermix_core::TaskArtifactType::File,
        };

        let result_ref = hermix_core::TaskResultRef {
            path: path.to_string(),
            artifact_type,
            summary: summary.map(String::from),
        };

        let task = self
            .service
            .add_result(task_id, result_ref)
            .map_err(map_error)?;
        Ok(task_to_info(&task))
    }
}
