//! Task orchestration tool with dependency tracking and execution feedback.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::tools::session_stamper::SessionStamper;
use crate::ToolRegistry;
use std::sync::Arc;

/// Channel capacity for the todo event bus. Mirrors
/// `hermix_gateway::groupchat::runtime::ROOM_EVENT_CHANNEL_CAPACITY`.
pub const TODO_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Per-tool broadcast payload. The wire format is `TodoSnapshotItem`
/// (defined in `hermix_transport`) — but we keep it as `serde_json::Value`
/// here to avoid hermix-tools depending on hermix-transport (that would
/// form a cycle hermix-server → hermix-tools → hermix-transport).
///
/// `session_id` is `None` for "no active session" (e.g. CLI mode) and
/// the WS layer must filter it out before forwarding to clients.
#[derive(Debug, Clone)]
pub struct TodoChange {
    pub session_id: Option<String>,
    pub change: TodoChangeKind,
}

#[derive(Debug, Clone)]
pub enum TodoChangeKind {
    Upsert(crate::backends::todo::TodoItem),
    Delete { id: String },
}

/// Register this tool with the registry.
pub fn register(
    registry: &ToolRegistry,
    event_tx: Option<broadcast::Sender<TodoChange>>,
) {
    let todo_path = hermix_config::paths::hermix_home().join("todos.json");
    let handler = Arc::new(TodoHandler::new(
        Arc::new(crate::backends::todo::FileTodoBackend::new(todo_path)),
        event_tx,
    ));
    let schema = handler.schema();
    // Keep a second typed handle for the session-stamper so the
    // registry can set the active session before each tool-call batch
    // without downcasting the `Arc<dyn ToolHandler>`.
    let stamper: Arc<dyn SessionStamper> = handler.clone() as Arc<dyn SessionStamper>;
    registry.register_with_risk(
        "todo",
        "todo",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Task orchestration: create, update, list, delete, get_next, depend, record_execution tasks.",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );
    registry.set_session_stamper("todo", stamper);
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
    /// Delete a task by id.
    async fn delete(&self, id: &str) -> Result<String, ToolError>;
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
    /// Optional event bus. When set, mutating tool actions broadcast
    /// a `TodoChange` so the front-end can render a live task panel.
    event_tx: Option<broadcast::Sender<TodoChange>>,
    /// Active session id for the **current** tool-call batch.
    /// Stamped onto every `TodoChange` emitted by `publish` so the
    /// WS layer can filter per session. Set via `set_active_session`
    /// from `agent_loop` before each tool-call batch and restored
    /// on scope exit. `std::sync::Mutex` is fine: publish is sync,
    /// the lock never crosses `.await`.
    active_session: std::sync::Mutex<Option<String>>,
}

impl TodoHandler {
    pub fn new(
        backend: Arc<dyn TodoBackend>,
        event_tx: Option<broadcast::Sender<TodoChange>>,
    ) -> Self {
        Self {
            backend,
            event_tx,
            active_session: std::sync::Mutex::new(None),
        }
    }

    /// Broadcast a TodoChange stamped with the **current** active
    /// session. No-op if no bus is attached. Errors (no subscribers /
    /// channel closed) are intentionally swallowed — the tool
    /// side-effect already succeeded, so we never want the broadcast
    /// failure to roll back the user's data.
    fn publish(&self, change: TodoChangeKind) {
        let session_id = self
            .active_session
            .lock()
            .expect("todo active_session mutex poisoned")
            .clone();
        if let Some(tx) = self.event_tx.as_ref() {
            let _ = tx.send(TodoChange { session_id, change });
        }
    }

    /// After a mutating action, re-`list` the backend and pull out the
    /// row matching `target_id`. This guarantees the broadcast carries
    /// the **authoritative current state** (including side effects
    /// from `record_execution` like `last_result` / `execution_count`).
    ///
    /// Returns `None` if the backend rejected the action or the row
    /// can't be found (callers should still publish a best-effort
    /// upsert from the input params in that case).
    async fn snapshot_after_mutation(
        &self,
        target_id: Option<&str>,
    ) -> Option<crate::backends::todo::TodoItem> {
        let list_json = self.backend.list(None).await.ok()?;
        let parsed: Value = serde_json::from_str(&list_json).ok()?;
        let arr = parsed.get("todos")?.as_array()?;
        let target = target_id?;
        for v in arr {
            if v.get("id").and_then(|x| x.as_str()) == Some(target) {
                return serde_json::from_value(v.clone()).ok();
            }
        }
        None
    }

    /// Build a minimal TodoItem from the **input params** (no backend
    /// round-trip). Used as a fallback when the backend's `list` fails
    /// or the just-mutated item is missing — best effort so the UI
    /// still shows *something* after the tool call.
    fn fallback_item_from_params(
        &self,
        action: &str,
        params: &Value,
    ) -> Option<crate::backends::todo::TodoItem> {
        let id = params.get("id").and_then(|v| v.as_str())?;
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending")
            .to_string();
        let priority = params
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("medium")
            .to_string();
        Some(crate::backends::todo::TodoItem {
            id: id.to_string(),
            title,
            description: None,
            status: if action == "create" { "pending".to_string() } else { status },
            priority,
            created_at: chrono::Utc::now().to_rfc3339(),
            depends_on: Vec::new(),
            last_result: None,
            execution_count: 0,
        })
    }
}

impl SessionStamper for TodoHandler {
    fn set_active_session(
        &self,
        session_id: Option<String>,
    ) -> Option<String> {
        let mut guard = self
            .active_session
            .lock()
            .expect("todo active_session mutex poisoned");
        std::mem::replace(&mut *guard, session_id)
    }
}

#[async_trait]
impl ToolHandler for TodoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        let result = match action {
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
            "delete" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.delete(id).await
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
        }?;

        // Broadcast mutation events AFTER the backend succeeds.
        // `list`, `get_next`, `get_dependencies` are pure reads — no event.
        if action == "delete" {
            // No snapshot needed — emit the removal so the WS forwarder
            // drops the row from the live TodoPanel.
            if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
                self.publish(TodoChangeKind::Delete {
                    id: id.to_string(),
                });
            }
        } else if matches!(action, "create" | "update" | "depend" | "record_execution") {
            // For "create" the id lives in the backend's response, not
            // the input params. Pull it out so we can re-`list`.
            let target_id: Option<String> = if action == "create" {
                serde_json::from_str::<Value>(&result)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
            } else {
                params.get("id").and_then(|v| v.as_str()).map(String::from)
            };

            let item = self
                .snapshot_after_mutation(target_id.as_deref())
                .await
                .or_else(|| self.fallback_item_from_params(action, &params));

            if let Some(item) = item {
                self.publish(TodoChangeKind::Upsert(item));
            }
        }

        Ok(result)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["create", "update", "list", "get_next", "delete", "depend", "record_execution", "get_dependencies"]
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
        let handler = TodoHandler::new(Arc::new(MockTodoBackend), None);
        let result = handler
            .execute(json!({"action": "create", "title": "Test task"}))
            .await
            .unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_todo_get_next() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend), None);
        let result = handler.execute(json!({"action": "get_next"})).await.unwrap();
        assert!(result.contains("No pending tasks"));
    }

    #[tokio::test]
    async fn test_todo_record_execution() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend), None);
        let result = handler
            .execute(json!({"action": "record_execution", "id": "test-id", "result": "success"}))
            .await
            .unwrap();
        assert!(result.contains("Recorded execution"));
    }

    #[tokio::test]
    async fn test_todo_schema() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend), None);
        assert_eq!(handler.schema().name, "todo");
    }
}
