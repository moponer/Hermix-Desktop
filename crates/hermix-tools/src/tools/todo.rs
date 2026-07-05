//! Task orchestration tool with dependency tracking and execution feedback.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry) {
    let todo_path = hermix_config::paths::hermix_home().join("todos.json");
    let handler = Arc::new(TodoHandler::new(Arc::new(
        crate::backends::todo::FileTodoBackend::new(todo_path),
    )));
    let schema = handler.schema();
    registry.register_with_risk(
        "todo",
        "todo",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Task orchestration: create, update, list, get_next, depend, record_execution tasks.",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// TodoBackend trait
// ---------------------------------------------------------------------------

/// Backend for task list management with orchestration support.
#[async_trait]
pub trait TodoBackend: Send + Sync {
    /// Create a new todo item.
    async fn create(
        &self,
        title: &str,
        description: Option<&str>,
        priority: Option<&str>,
    ) -> Result<String, ToolError>;
    /// Update a todo item.
    async fn update(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<String, ToolError>;
    /// List all todo items.
    async fn list(&self, status: Option<&str>) -> Result<String, ToolError>;
    /// Add dependency: task_id depends on depends_on tasks.
    async fn add_dependency(&self, task_id: &str, depends_on: &[String]) -> Result<String, ToolError>;
    /// Get the next task to execute (respecting dependencies and priority).
    async fn get_next(&self) -> Result<String, ToolError>;
    /// Record execution result for a task.
    async fn record_execution(&self, task_id: &str, result: &str) -> Result<String, ToolError>;
    /// Get dependencies for a task.
    async fn get_dependencies(&self, task_id: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// TodoHandler
// ---------------------------------------------------------------------------

/// Task orchestration tool with dependency tracking and execution feedback.
pub struct TodoHandler {
    backend: Arc<dyn TodoBackend>,
}

impl TodoHandler {
    pub fn new(backend: Arc<dyn TodoBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for TodoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'title' parameter".into()))?;
                let description = params.get("description").and_then(|v| v.as_str());
                let priority = params.get("priority").and_then(|v| v.as_str());
                self.backend.create(title, description, priority).await
            }
            "update" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let title = params.get("title").and_then(|v| v.as_str());
                let description = params.get("description").and_then(|v| v.as_str());
                let status = params.get("status").and_then(|v| v.as_str());
                let priority = params.get("priority").and_then(|v| v.as_str());
                self.backend
                    .update(id, title, description, status, priority)
                    .await
            }
            "list" => {
                let status = params.get("status").and_then(|v| v.as_str());
                self.backend.list(status).await
            }
            "get_next" => {
                self.backend.get_next().await
            }
            "depend" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let depends_on: Vec<String> = params
                    .get("depends_on")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if depends_on.is_empty() {
                    return Err(ToolError::InvalidParams("Missing 'depends_on' parameter (array of task IDs)".into()));
                }
                self.backend.add_dependency(id, &depends_on).await
            }
            "record_execution" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let result = params
                    .get("result")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'result' parameter".into()))?;
                self.backend.record_execution(id, result).await
            }
            "get_dependencies" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.get_dependencies(id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use: create, update, list, get_next, depend, record_execution, get_dependencies.",
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
                "enum": ["create", "update", "list", "get_next", "depend", "record_execution", "get_dependencies"]
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Task ID (for update, depend, record_execution, get_dependencies)"
            }),
        );
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Task title (for create/update)"
            }),
        );
        props.insert(
            "description".into(),
            json!({
                "type": "string",
                "description": "Task description (for create/update)"
            }),
        );
        props.insert("status".into(), json!({
            "type": "string",
            "description": "Task status: pending, in_progress, completed, failed (for update/list filter)",
            "enum": ["pending", "in_progress", "completed", "failed"]
        }));
        props.insert(
            "priority".into(),
            json!({
                "type": "string",
                "description": "Priority level: low, medium, high",
                "enum": ["low", "medium", "high"]
            }),
        );
        props.insert(
            "depends_on".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Array of task IDs this task depends on (for depend action)"
            }),
        );
        props.insert(
            "result".into(),
            json!({
                "type": "string",
                "description": "Execution result (for record_execution action)"
            }),
        );

        tool_schema(
            "todo",
            "Task orchestration with dependency tracking. Use get_next to find the next task to execute based on dependencies and priority. After executing a task, use record_execution to log the result and update status to completed.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTodoBackend;
    #[async_trait]
    impl TodoBackend for MockTodoBackend {
        async fn create(
            &self,
            title: &str,
            _description: Option<&str>,
            _priority: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Created todo: {}", title))
        }
        async fn update(
            &self,
            id: &str,
            _title: Option<&str>,
            _description: Option<&str>,
            _status: Option<&str>,
            _priority: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Updated todo: {}", id))
        }
        async fn list(&self, _status: Option<&str>) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn add_dependency(&self, task_id: &str, _depends_on: &[String]) -> Result<String, ToolError> {
            Ok(format!("Added dependency to: {}", task_id))
        }
        async fn get_next(&self) -> Result<String, ToolError> {
            Ok(json!({"id": null, "message": "No pending tasks"}).to_string())
        }
        async fn record_execution(&self, task_id: &str, _result: &str) -> Result<String, ToolError> {
            Ok(format!("Recorded execution for: {}", task_id))
        }
        async fn get_dependencies(&self, task_id: &str) -> Result<String, ToolError> {
            Ok(json!({"task_id": task_id, "dependencies": []}).to_string())
        }
    }

    #[tokio::test]
    async fn test_todo_create() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        let result = handler
            .execute(json!({"action": "create", "title": "Test task"}))
            .await
            .unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_todo_get_next() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        let result = handler.execute(json!({"action": "get_next"})).await.unwrap();
        assert!(result.contains("No pending tasks"));
    }

    #[tokio::test]
    async fn test_todo_record_execution() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        let result = handler
            .execute(json!({"action": "record_execution", "id": "test-id", "result": "success"}))
            .await
            .unwrap();
        assert!(result.contains("Recorded execution"));
    }

    #[tokio::test]
    async fn test_todo_schema() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        assert_eq!(handler.schema().name, "todo");
    }
}
