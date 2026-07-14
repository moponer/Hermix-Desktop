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
    /// LLM declared a new plan. Emitted on the first `todo.create`
    /// for a fresh `group_id`. The front-end creates a new inline
    /// card anchored to the assistant message that emitted this
    /// event.
    GroupStarted {
        group_id: String,
        group_title: Option<String>,
        /// Wire-format id of the assistant message that produced
        /// this group (`${session_id}-${rowIdx}`). Stamped from
        /// `set_active_message_id` at the top of the turn so the
        /// front-end can anchor the inline card next to the right
        /// assistant message even after reload (the same formula
        /// reproduces the same id from the persisted row order).
        /// `None` for CLI / legacy paths that have no message
        /// context.
        message_id: Option<String>,
    },
    /// Single-item upsert (create / update / depend / record_execution).
    Upsert(crate::backends::todo::TodoItem),
    /// Single-item removal.
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
    ///
    /// `group_id` and `group_title` let the LLM attach this task to an
    /// existing plan (`group_id`) or start a new one (`group_title`).
    /// When neither is provided, the backend synthesizes a fresh
    /// group_id so each call lands in its own plan card.
    async fn create(
        &self,
        title: &str,
        description: Option<&str>,
        priority: Option<&str>,
        group_id: Option<&str>,
        group_title: Option<&str>,
        // Session id this todo belongs to. `None` for CLI / legacy
        // paths that have no session context. Persisted on the
        // `TodoItem` so `todo_snapshot` WS hydrate can serve
        // per-session lists. FRONTEND_AUDIT_TODO_2026-07-13.
        session_id: Option<&str>,
        // Wire-format id of the assistant message that emitted the
        // `todo.create` tool call (`${session_id}-${rowIdx}`).
        // Stamped from `set_active_message_id` at the top of the
        // turn. Anchors the inline plan card to the originating
        // assistant message after reload. `None` for legacy paths.
        // P0-T1.
        origin_message_id: Option<&str>,
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
    /// List todos belonging to a specific session. Default impl falls
    /// back to `list(None)` so legacy backends (no session concept)
    /// keep working; backends with `TodoItem.session_id` should
    /// override for the per-session filter. Used by `todo_snapshot`
    /// WS hydrate on connect + after every mutation. Commit A.
    async fn list_for_session(&self, session_id: &str) -> Result<String, ToolError> {
        // Default: ignore session, return everything. The file
        // backend overrides this to actually filter.
        let _ = session_id;
        self.list(None).await
    }
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
    /// Active assistant message id for the **current** tool-call
    /// batch. Stamped onto `TodoChangeKind::GroupStarted` so the
    /// front-end can anchor the inline card to the originating
    /// assistant message. Set via `set_active_message_id` from
    /// `agent_loop` before each tool-call batch.
    /// P0-T1 (Frontend Audit 2026-07-13).
    active_message_id: std::sync::Mutex<Option<String>>,
    /// Group ids we have already announced via `TodoChangeKind::GroupStarted`
    /// for this process lifetime. Ensures the front-end gets exactly one
    /// `GroupStarted` per plan, not one per `create` call within the plan.
    /// Reset is intentionally implicit (process restart) so a refresh of
    /// the backend still re-fans-out the surviving groups on next create.
    announced_groups: std::sync::Mutex<std::collections::HashSet<String>>,
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
            active_message_id: std::sync::Mutex::new(None),
            announced_groups: std::sync::Mutex::new(std::collections::HashSet::new()),
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

    /// Emit a `GroupStarted` for a freshly-created plan (first time we
    /// see this `group_id`). Subsequent calls for the same id are
    /// no-ops. Caller must already have a successful backend response
    /// (so the id is real and persisted).
    fn announce_group(&self, group_id: &str, group_title: Option<String>) {
        let mut seen = self
            .announced_groups
            .lock()
            .expect("announced_groups mutex poisoned");
        if !seen.insert(group_id.to_string()) {
            return; // already announced
        }
        drop(seen);
        let message_id = self
            .active_message_id
            .lock()
            .expect("todo active_message_id mutex poisoned")
            .clone();
        self.publish(TodoChangeKind::GroupStarted {
            group_id: group_id.to_string(),
            group_title,
            message_id,
        });
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
        // Snapshot the active session/message id **once** so the
        // fallback path doesn't have to take the locks twice. Commit A.
        let active_session_id = self
            .active_session
            .lock()
            .expect("todo active_session mutex poisoned")
            .clone();
        let active_message_id = self
            .active_message_id
            .lock()
            .expect("todo active_message_id mutex poisoned")
            .clone();

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
            group_id: params.get("group_id").and_then(|v| v.as_str()).map(String::from).unwrap_or_default(),
            group_title: params.get("group_title").and_then(|v| v.as_str()).map(String::from),
            // FRONTEND_AUDIT_TODO_2026-07-13 / Commit A: stamp
            // session/message context from the active stamper so the
            // fallback snapshot still carries the anchor + session id.
            session_id: active_session_id,
            origin_message_id: active_message_id,
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

    fn set_active_message_id(
        &self,
        message_id: Option<String>,
    ) -> Option<String> {
        let mut guard = self
            .active_message_id
            .lock()
            .expect("todo active_message_id mutex poisoned");
        std::mem::replace(&mut *guard, message_id)
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
                let group_id = params.get("group_id").and_then(|v| v.as_str());
                let group_title = params.get("group_title").and_then(|v| v.as_str());
                // FRONTEND_AUDIT_TODO_2026-07-13 / Commit A: stamp the
                // active session + assistant-message id so the new
                // TodoItem is owned by the current conversation and
                // anchored to the message that produced it.
                let active_session_id = self
                    .active_session
                    .lock()
                    .expect("todo active_session mutex poisoned")
                    .clone();
                let active_message_id = self
                    .active_message_id
                    .lock()
                    .expect("todo active_message_id mutex poisoned")
                    .clone();
                self.backend
                    .create(
                        title,
                        description,
                        priority,
                        group_id,
                        group_title,
                        active_session_id.as_deref(),
                        active_message_id.as_deref(),
                    )
                    .await
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
                // If this item belongs to a plan we have not announced yet
                // (LLM started a brand-new plan in this turn, or this is the
                // first create after a process restart for an existing plan),
                // emit a GroupStarted so the front-end can mount a fresh
                // inline card anchored to the originating message.
                if !item.group_id.is_empty() {
                    self.announce_group(&item.group_id, item.group_title.clone());
                }
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
            "group_id".into(),
            json!({
                "type": "string",
                "description": "Plan id to attach this task to. Omit (or pass the same id from an earlier response) to extend an existing plan. When starting a brand-new plan, set group_title instead and let the backend mint a fresh id."
            }),
        );
        props.insert(
            "group_title".into(),
            json!({
                "type": "string",
                "description": "Human label for a new plan (for example rename all files). Pass this on the FIRST create of a new plan; subsequent creates in the same plan should reuse the returned group_id and omit group_title."
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
            _group_id: Option<&str>,
            _group_title: Option<&str>,
            _session_id: Option<&str>,
            _origin_message_id: Option<&str>,
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
        async fn delete(&self, _id: &str) -> Result<String, ToolError> {
            Ok(format!("Deleted todo"))
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

    /// P0-T1 (Frontend Audit 2026-07-13) regression: when an
    /// `assistant_message_id` is set via the SessionStamper before
    /// the first `todo.create`, the emitted `TodoChangeKind::GroupStarted`
    /// must carry that id so the WS forwarder can stamp the front-end
    /// event with it (and `ChatView` can anchor the inline card to the
    /// correct assistant message instead of using the fragile
    /// created_at fallback).
    #[tokio::test]
    async fn test_todo_group_started_carries_active_message_id() {
        use crate::tools::session_stamper::SessionStamper;
        use crate::tools::todo::TodoChangeKind;

        // Backend that returns a real `id` so the post-create upsert
        // path triggers `announce_group` → `publish(GroupStarted)`.
        struct BackendWithId;
        #[async_trait]
        impl TodoBackend for BackendWithId {
            async fn create(
                &self,
                title: &str,
                _description: Option<&str>,
                _priority: Option<&str>,
                _group_id: Option<&str>,
                _group_title: Option<&str>,
                _session_id: Option<&str>,
                _origin_message_id: Option<&str>,
            ) -> Result<String, ToolError> {
                Ok(json!({"id": "todo-1", "title": title}).to_string())
            }
            async fn update(
                &self,
                _id: &str,
                _title: Option<&str>,
                _description: Option<&str>,
                _status: Option<&str>,
                _priority: Option<&str>,
            ) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn list(&self, _status: Option<&str>) -> Result<String, ToolError> {
                Ok(json!({"todos": [{
                    "id": "todo-1",
                    "title": "x",
                    "description": null,
                    "status": "pending",
                    "priority": "medium",
                    "created_at": "2026-07-13T00:00:00Z",
                    "depends_on": [],
                    "last_result": null,
                    "execution_count": 0,
                    "group_id": "group-1",
                    "group_title": "My plan",
                }]}).to_string())
            }
            async fn add_dependency(&self, _: &str, _: &[String]) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn get_next(&self) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn delete(&self, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn record_execution(&self, _: &str, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn get_dependencies(&self, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
        }

        let (tx, mut rx) = broadcast::channel::<TodoChange>(16);
        let handler = Arc::new(TodoHandler::new(Arc::new(BackendWithId), Some(tx)));

        // Stamp active session + assistant_message_id (the wire-format
        // `${session_id}-${rowIdx}` formula). Both must show up on the
        // broadcast.
        let stamper: Arc<dyn SessionStamper> = handler.clone() as Arc<dyn SessionStamper>;
        let prev_sess = stamper.set_active_session(Some("sess-1".to_string()));
        assert_eq!(prev_sess, None, "first stamp should be None");
        let prev_msg =
            stamper.set_active_message_id(Some("sess-1-3".to_string()));
        assert_eq!(prev_msg, None, "first message_id stamp should be None");

        // Trigger `GroupStarted` by creating the first todo in a new group.
        handler
            .execute(json!({
                "action": "create",
                "title": "Do the thing",
                "group_title": "My plan",
            }))
            .await
            .unwrap();

        // Drain the broadcast — first event should be GroupStarted.
        let evt = rx.recv().await.unwrap();
        assert_eq!(evt.session_id.as_deref(), Some("sess-1"));
        match evt.change {
            TodoChangeKind::GroupStarted { group_id, group_title, message_id } => {
                assert_eq!(group_id, "group-1");
                assert_eq!(group_title.as_deref(), Some("My plan"));
                assert_eq!(
                    message_id.as_deref(),
                    Some("sess-1-3"),
                    "GroupStarted must carry the assistant_message_id \
                     stamped via set_active_message_id so the front-end \
                     can anchor the inline card to the right message"
                );
            }
            other => panic!("expected GroupStarted, got {:?}", other),
        }
    }

    /// P0-T1 follow-up: when no message_id is set (legacy CLI / HTTP
    /// paths), `GroupStarted.message_id` is `None` — the front-end
    /// falls back to the timestamp-based anchoring path.
    #[tokio::test]
    async fn test_todo_group_started_message_id_none_when_unset() {
        use crate::tools::session_stamper::SessionStamper;
        use crate::tools::todo::TodoChangeKind;

        struct BackendWithId;
        #[async_trait]
        impl TodoBackend for BackendWithId {
            async fn create(
                &self,
                title: &str,
                _description: Option<&str>,
                _priority: Option<&str>,
                _group_id: Option<&str>,
                _group_title: Option<&str>,
                _session_id: Option<&str>,
                _origin_message_id: Option<&str>,
            ) -> Result<String, ToolError> {
                Ok(json!({"id": "todo-1", "title": title}).to_string())
            }
            async fn update(
                &self,
                _id: &str,
                _title: Option<&str>,
                _description: Option<&str>,
                _status: Option<&str>,
                _priority: Option<&str>,
            ) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn list(&self, _status: Option<&str>) -> Result<String, ToolError> {
                Ok(json!({"todos": [{
                    "id": "todo-1",
                    "title": "x",
                    "description": null,
                    "status": "pending",
                    "priority": "medium",
                    "created_at": "2026-07-13T00:00:00Z",
                    "depends_on": [],
                    "last_result": null,
                    "execution_count": 0,
                    "group_id": "group-1",
                    "group_title": null,
                }]}).to_string())
            }
            async fn add_dependency(&self, _: &str, _: &[String]) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn get_next(&self) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn delete(&self, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn record_execution(&self, _: &str, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
            async fn get_dependencies(&self, _: &str) -> Result<String, ToolError> {
                Ok("{}".to_string())
            }
        }

        let (tx, mut rx) = broadcast::channel::<TodoChange>(16);
        let handler = Arc::new(TodoHandler::new(Arc::new(BackendWithId), Some(tx)));

        // Only stamp the session, NOT the message_id (legacy path).
        let stamper: Arc<dyn SessionStamper> = handler.clone() as Arc<dyn SessionStamper>;
        stamper.set_active_session(Some("sess-2".to_string()));
        // Deliberately do NOT call set_active_message_id.

        handler
            .execute(json!({
                "action": "create",
                "title": "Plain",
                "group_title": null,
            }))
            .await
            .unwrap();

        let evt = rx.recv().await.unwrap();
        assert_eq!(evt.session_id.as_deref(), Some("sess-2"));
        match evt.change {
            TodoChangeKind::GroupStarted { message_id, .. } => {
                assert!(
                    message_id.is_none(),
                    "GroupStarted.message_id must be None when no message_id was stamped; \
                     legacy / CLI / HTTP paths don't have a message context"
                );
            }
            other => panic!("expected GroupStarted, got {:?}", other),
        }
    }

    /// Commit A regression: `todo.create` must thread the active
    /// session id and assistant-message id into the persisted
    /// `TodoItem` so the `todo_snapshot` WS hydrate can serve
    /// per-session lists and re-anchor inline cards after reload.
    /// Backed by a real `FileTodoBackend` writing to a temp file.
    #[tokio::test]
    async fn test_todo_create_persists_session_and_message_id() {
        use crate::backends::todo::FileTodoBackend;
        use crate::tools::session_stamper::SessionStamper;
        use crate::tools::todo::TodoBackend as _;
        let tmp = tempfile::tempdir().unwrap();
        let backend_path = tmp.path().join("todos.json");
        let backend = Arc::new(FileTodoBackend::new(backend_path));
        let (tx, _rx) = broadcast::channel::<TodoChange>(16);
        let handler = Arc::new(TodoHandler::new(backend.clone(), Some(tx)));

        // Stamp active session + message_id before the create.
        let stamper: Arc<dyn SessionStamper> = handler.clone() as Arc<dyn SessionStamper>;
        stamper.set_active_session(Some("sess-A".to_string()));
        stamper.set_active_message_id(Some("sess-A-7".to_string()));

        handler
            .execute(json!({
                "action": "create",
                "title": "Persisted with context",
                "group_title": "Persistence plan",
            }))
            .await
            .unwrap();

        // Re-list the backend and confirm the persisted TodoItem
        // carries both ids.
        let raw = backend.list(None).await.unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        let arr = parsed.get("todos").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1, "expected 1 todo after create");
        let item = &arr[0];
        assert_eq!(
            item.get("session_id").and_then(|v| v.as_str()),
            Some("sess-A"),
            "session_id must be stamped from active stamper"
        );
        assert_eq!(
            item.get("origin_message_id").and_then(|v| v.as_str()),
            Some("sess-A-7"),
            "origin_message_id must be stamped from active stamper"
        );
    }

    /// Commit A regression: `list_for_session` filters out todos that
    /// belong to other sessions. Without this the front-end todo
    /// panel would render every session's tasks in every chat.
    #[tokio::test]
    async fn test_list_for_session_isolates_by_session() {
        use crate::backends::todo::FileTodoBackend;
        use crate::tools::todo::TodoBackend as _;
        let tmp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FileTodoBackend::new(tmp.path().join("todos.json")));

        // Seed two items in different sessions + one unowned.
        backend
            .create(
                "alpha",
                None,
                None,
                Some("g1"),
                Some("plan1"),
                Some("sess-X"),
                Some("sess-X-0"),
            )
            .await
            .unwrap();
        backend
            .create(
                "beta",
                None,
                None,
                Some("g2"),
                Some("plan2"),
                Some("sess-Y"),
                Some("sess-Y-0"),
            )
            .await
            .unwrap();
        backend
            .create("orphan", None, None, Some("g3"), Some("plan3"), None, None)
            .await
            .unwrap();

        let x_raw = backend.list_for_session("sess-X").await.unwrap();
        let x: Value = serde_json::from_str(&x_raw).unwrap();
        let x_items = x.get("todos").and_then(|v| v.as_array()).unwrap();
        assert_eq!(x_items.len(), 1, "sess-X should only see alpha");
        assert_eq!(
            x_items[0].get("title").and_then(|v| v.as_str()),
            Some("alpha")
        );

        let y_raw = backend.list_for_session("sess-Y").await.unwrap();
        let y: Value = serde_json::from_str(&y_raw).unwrap();
        let y_items = y.get("todos").and_then(|v| v.as_array()).unwrap();
        assert_eq!(y_items.len(), 1, "sess-Y should only see beta");
        assert_eq!(
            y_items[0].get("title").and_then(|v| v.as_str()),
            Some("beta")
        );

        // Unknown session → empty list (NOT the orphan).
        let none_raw = backend.list_for_session("sess-Z").await.unwrap();
        let none: Value = serde_json::from_str(&none_raw).unwrap();
        let none_items = none.get("todos").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            none_items.len(),
            0,
            "unknown session must not see orphan (no session_id)"
        );
    }
}
