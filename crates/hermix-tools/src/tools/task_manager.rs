//! Task management tool — background task lifecycle.
//!
//! Inspired by claw-code's TaskCreate/TaskGet/TaskList/TaskStop/TaskUpdate/TaskOutput
//! tools. Provides a unified interface for managing background tasks.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::{RiskLevel, PermissionMode}};

use crate::ToolRegistry;

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(TaskManagerHandler::new());
    let schema = handler.schema();
    registry.register_with_permission(
        "task_manager",
        "task",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Manage background tasks: create, get status, list, stop, update, get output.",
        "📋",
        None,
        RiskLevel::Safe,
        PermissionMode::WorkspaceWrite,
        false,
    );
}

// ---------------------------------------------------------------------------
// TaskState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub prompt: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

// ---------------------------------------------------------------------------
// TaskManagerHandler
// ---------------------------------------------------------------------------

pub struct TaskManagerHandler {
    tasks: Mutex<Vec<TaskInfo>>,
}

impl TaskManagerHandler {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ToolHandler for TaskManagerHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let prompt = params
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt' parameter".into()))?;
                let description = params.get("description").and_then(|v| v.as_str());
                
                let mut tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let id = uuid::Uuid::new_v4().to_string();
                let task = TaskInfo {
                    id: id.clone(),
                    prompt: prompt.to_string(),
                    description: description.map(String::from),
                    status: TaskStatus::Pending,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    started_at: None,
                    completed_at: None,
                    output: None,
                    error: None,
                };
                tasks.push(task);
                
                Ok(json!({
                    "status": "created",
                    "task_id": id,
                    "prompt": prompt,
                    "message": format!("Task created. Use task_manager(action='get', task_id='{}') to check status.", id)
                }).to_string())
            }
            "get" => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;
                
                let tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let task = tasks.iter().find(|t| t.id == task_id)
                    .ok_or_else(|| ToolError::NotFound(format!("Task '{}' not found", task_id)))?;
                
                Ok(json!(task).to_string())
            }
            "list" => {
                let status_filter = params.get("status").and_then(|v| v.as_str());
                let tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                
                let filtered: Vec<&TaskInfo> = match status_filter {
                    Some(s) => tasks.iter().filter(|t| {
                        let status_str = serde_json::to_string(&t.status).unwrap_or_default();
                        status_str.trim_matches('"') == s
                    }).collect(),
                    None => tasks.iter().collect(),
                };
                
                Ok(json!({
                    "tasks": filtered,
                    "total": filtered.len()
                }).to_string())
            }
            "stop" => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;
                
                let mut tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let task = tasks.iter_mut().find(|t| t.id == task_id)
                    .ok_or_else(|| ToolError::NotFound(format!("Task '{}' not found", task_id)))?;
                
                if task.status == TaskStatus::Running {
                    task.status = TaskStatus::Stopped;
                    task.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    Ok(json!({"status": "stopped", "task_id": task_id}).to_string())
                } else {
                    Err(ToolError::InvalidParams(format!(
                        "Task '{}' is not running (status: {:?})",
                        task_id, task.status
                    )))
                }
            }
            "update" => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;
                let status = params.get("status").and_then(|v| v.as_str());
                let output = params.get("output").and_then(|v| v.as_str());
                let error = params.get("error").and_then(|v| v.as_str());
                
                let mut tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let task = tasks.iter_mut().find(|t| t.id == task_id)
                    .ok_or_else(|| ToolError::NotFound(format!("Task '{}' not found", task_id)))?;
                
                if let Some(s) = status {
                    task.status = match s {
                        "running" => TaskStatus::Running,
                        "completed" => TaskStatus::Completed,
                        "failed" => TaskStatus::Failed,
                        "stopped" => TaskStatus::Stopped,
                        _ => return Err(ToolError::InvalidParams(format!("Invalid status: {}", s))),
                    };
                    if task.status == TaskStatus::Running && task.started_at.is_none() {
                        task.started_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                    if matches!(task.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped) {
                        task.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                }
                if let Some(o) = output {
                    task.output = Some(o.to_string());
                }
                if let Some(e) = error {
                    task.error = Some(e.to_string());
                }
                
                Ok(json!({"status": "updated", "task_id": task_id}).to_string())
            }
            "output" => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task_id' parameter".into()))?;
                
                let tasks = self.tasks.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let task = tasks.iter().find(|t| t.id == task_id)
                    .ok_or_else(|| ToolError::NotFound(format!("Task '{}' not found", task_id)))?;
                
                Ok(json!({
                    "task_id": task_id,
                    "status": task.status,
                    "output": task.output,
                    "error": task.error
                }).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use: create, get, list, stop, update, output.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["create", "get", "list", "stop", "update", "output"]
            }),
        );
        props.insert(
            "task_id".into(),
            json!({
                "type": "string",
                "description": "Task ID (for get, stop, update, output)"
            }),
        );
        props.insert(
            "prompt".into(),
            json!({
                "type": "string",
                "description": "Task prompt/description (for create)"
            }),
        );
        props.insert(
            "description".into(),
            json!({
                "type": "string",
                "description": "Additional task description (for create)"
            }),
        );
        props.insert(
            "status".into(),
            json!({
                "type": "string",
                "description": "Task status: pending, running, completed, failed, stopped",
                "enum": ["pending", "running", "completed", "failed", "stopped"]
            }),
        );
        props.insert(
            "output".into(),
            json!({
                "type": "string",
                "description": "Task output (for update)"
            }),
        );
        props.insert(
            "error".into(),
            json!({
                "type": "string",
                "description": "Task error message (for update)"
            }),
        );

        tool_schema(
            "task_manager",
            "Manage background tasks. Create long-running tasks, check their status, stop running tasks, or retrieve their output.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}
