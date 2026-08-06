//! Real todo backend: JSON file-based task list with task orchestration.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Mutex;

use crate::tools::todo::TodoBackend;
use hermix_core::ToolError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub created_at: String,
    /// Task IDs this task depends on (empty = no dependencies).
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Result from last execution (for feedback loop).
    #[serde(default)]
    pub last_result: Option<String>,
    /// Execution count (how many times this task was attempted).
    #[serde(default)]
    pub execution_count: u32,
    /// Plan / group id. Every todo belongs to exactly one plan.
    /// `#[serde(default)]` so legacy items deserialize into an empty string.
    #[serde(default)]
    pub group_id: String,
    /// Human label for the plan (e.g. "rename all files"). Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_title: Option<String>,
    /// Session id this todo was created in. Used by the front-end
    /// todo panel + WS `todo_snapshot` to filter per-session.
    /// `None` for pre-session-context items (legacy CLI). `#[serde(default)]`
    /// keeps old `todos.json` files compatible — they decode as `None`.
    /// Commit A.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Wire-format assistant message id that emitted the tool call
    /// which created this todo (`${session_id}-${rowIdx}`). Anchors
    /// the inline plan card next to the right assistant message
    /// after reload. `#[serde(default)]` for legacy compatibility.
    /// .
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_message_id: Option<String>,
}

/// File-based todo backend storing tasks as JSON.
pub struct FileTodoBackend {
    file_path: std::path::PathBuf,
    items: Mutex<Vec<TodoItem>>,
}

impl FileTodoBackend {
    pub fn new(file_path: std::path::PathBuf) -> Self {
        let items = if file_path.exists() {
            std::fs::read_to_string(&file_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            file_path,
            items: Mutex::new(items),
        }
    }

    pub fn default_path() -> Self {
        let path = hermix_config::paths::hermix_home().join("todos.json");
        Self::new(path)
    }

    fn save(&self, items: &[TodoItem]) -> Result<(), ToolError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create dir: {}", e)))?;
        }
        let json = serde_json::to_string_pretty(items)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize todos: {}", e)))?;
        std::fs::write(&self.file_path, json)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write todos: {}", e)))?;
        Ok(())
    }

    /// Get the next task to execute based on dependencies and priority.
    fn get_next_task(items: &[TodoItem]) -> Option<&TodoItem> {
        let pending: Vec<&TodoItem> = items.iter().filter(|i| i.status == "pending").collect();

        // First pass: find tasks with all dependencies satisfied
        for task in &pending {
            let deps_satisfied = task.depends_on.iter().all(|dep_id| {
                items.iter().any(|i| i.id == *dep_id && i.status == "completed")
            });
            if deps_satisfied {
                return Some(task);
            }
        }

        // Second pass: return highest priority with fewest unmet dependencies
        pending
            .iter()
            .min_by_key(|t| {
                let unmet = t.depends_on.iter().filter(|dep_id| {
                    !items.iter().any(|i| i.id == **dep_id && i.status == "completed")
                }).count();
                (unmet, priority_to_num(&t.priority))
            })
            .copied()
    }
}

fn priority_to_num(p: &str) -> u8 {
    match p {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 1,
    }
}

#[async_trait]
impl TodoBackend for FileTodoBackend {
    async fn create(
        &self,
        title: &str,
        description: Option<&str>,
        priority: Option<&str>,
        group_id: Option<&str>,
        group_title: Option<&str>,
        session_id: Option<&str>,
        origin_message_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
        // If group_id is missing, fall back to the active group_id or
        // synthesize a new one. This keeps old LLM behavior (no group)
        // working while letting LLM explicitly start a new plan.
        //
        // 主人要求（2026-08-05）：同一批次的任务必须在一个面板里。
        // LLM 常在同一轮回复里连发多个 create（拆步骤），若每次都
        // mint 新 group，前端会渲染出 N 张独立卡片。这里改为：
        // 同一 `origin_message_id`（同一轮回复）内已存在的 group 直接
        // 复用；只有跨回复（新 message）且未指定 group_id 时才开新组。
        let resolved_group_id = match group_id {
            Some(g) if !g.is_empty() => g.to_string(),
            _ => {
                let same_turn_group = origin_message_id
                    .and_then(|mid| {
                        items
                            .iter()
                            .rev()
                            .find(|i| {
                                i.origin_message_id.as_deref() == Some(mid)
                                    && !i.group_id.is_empty()
                            })
                            .map(|i| i.group_id.clone())
                    });
                match same_turn_group {
                    Some(g) => g,
                    None => uuid::Uuid::new_v4().to_string(),
                }
            }
        };
        let item = TodoItem {
            id: id.clone(),
            title: title.to_string(),
            description: description.map(String::from),
            status: "pending".to_string(),
            priority: priority.unwrap_or("medium").to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            depends_on: Vec::new(),
            last_result: None,
            execution_count: 0,
            group_id: resolved_group_id.clone(),
            group_title: group_title.map(String::from),
            session_id: session_id.map(String::from),
            origin_message_id: origin_message_id.map(String::from),
        };
        items.push(item.clone());
        self.save(&items)?;
        Ok(json!({
            "status": "ok",
            "id": id,
            "title": title,
            "group_id": resolved_group_id,
            "group_title": group_title,
        }).to_string())
    }

    async fn update(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let item = items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", id)))?;

        if let Some(t) = title {
            item.title = t.to_string();
        }
        if let Some(d) = description {
            item.description = Some(d.to_string());
        }
        if let Some(s) = status {
            item.status = s.to_string();
        }
        if let Some(p) = priority {
            item.priority = p.to_string();
        }

        self.save(&items)?;
        Ok(json!({"status": "ok", "id": id, "action": "updated"}).to_string())
    }

    async fn list(&self, status: Option<&str>) -> Result<String, ToolError> {
        let items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let filtered: Vec<&TodoItem> = match status {
            Some(s) => items.iter().filter(|i| i.status == s).collect(),
            None => items.iter().collect(),
        };
        let result: Vec<serde_json::Value> = filtered
            .iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "title": i.title,
                    "description": i.description,
                    "status": i.status,
                    "priority": i.priority,
                    "created_at": i.created_at,
                    "depends_on": i.depends_on,
                    "last_result": i.last_result,
                    "execution_count": i.execution_count,
                    "group_id": i.group_id,
                    "group_title": i.group_title,
                    "session_id": i.session_id,
                    "origin_message_id": i.origin_message_id,
                })
            })
            .collect();
        Ok(json!({"todos": result, "total": result.len()}).to_string())
    }

    /// List todos belonging to a specific session. Used by the
    /// `todo_snapshot` WS hydrate + the front-end panel refresh path.
    /// Default impl falls back to `list(None)` for legacy in-memory
    /// backends that don't track sessions; backends with session
    /// metadata should override for the per-session filter.
    /// Commit A.
    async fn list_for_session(&self, session_id: &str) -> Result<String, ToolError> {
        let items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let filtered: Vec<&TodoItem> = items
            .iter()
            .filter(|i| i.session_id.as_deref() == Some(session_id))
            .collect();
        let result: Vec<serde_json::Value> = filtered
            .iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "title": i.title,
                    "description": i.description,
                    "status": i.status,
                    "priority": i.priority,
                    "created_at": i.created_at,
                    "depends_on": i.depends_on,
                    "last_result": i.last_result,
                    "execution_count": i.execution_count,
                    "group_id": i.group_id,
                    "group_title": i.group_title,
                    "session_id": i.session_id,
                    "origin_message_id": i.origin_message_id,
                })
            })
            .collect();
        Ok(json!({
            "todos": result,
            "total": result.len(),
            "session_id": session_id,
        })
        .to_string())
    }

    async fn add_dependency(&self, task_id: &str, depends_on: &[String]) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Verify all dependencies exist
        for dep_id in depends_on {
            if !items.iter().any(|i| i.id == *dep_id) {
                return Err(ToolError::NotFound(format!("Dependency '{}' not found", dep_id)));
            }
        }

        // Find and update the task
        let task_idx = items.iter().position(|i| i.id == task_id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", task_id)))?;

        for dep_id in depends_on {
            if !items[task_idx].depends_on.contains(dep_id) {
                items[task_idx].depends_on.push(dep_id.clone());
            }
        }

        let depends_on_clone = items[task_idx].depends_on.clone();
        self.save(&items)?;
        Ok(json!({"status": "ok", "id": task_id, "depends_on": depends_on_clone}).to_string())
    }

    async fn get_next(&self) -> Result<String, ToolError> {
        let items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        match Self::get_next_task(&items) {
            Some(task) => {
                let id = task.id.clone();
                let title = task.title.clone();
                let description = task.description.clone();
                let priority = task.priority.clone();
                let depends_on = task.depends_on.clone();
                let execution_count = task.execution_count;
                Ok(json!({
                    "id": id,
                    "title": title,
                    "description": description,
                    "priority": priority,
                    "depends_on": depends_on,
                    "execution_count": execution_count,
                    "message": "Execute this task next. After completion, update status to 'completed' and call record_execution."
                }).to_string())
            }
            None => Ok(json!({
                "id": null,
                "message": "No pending tasks available. All tasks are completed or blocked by unmet dependencies."
            }).to_string()),
        }
    }

    async fn delete(&self, id: &str) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let idx = items
            .iter()
            .position(|i| i.id == id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", id)))?;
        items.remove(idx);
        self.save(&items)?;
        Ok(json!({"status": "ok", "id": id, "action": "deleted"}).to_string())
    }

    async fn record_execution(&self, task_id: &str, result: &str) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let task_idx = items.iter().position(|i| i.id == task_id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", task_id)))?;

        items[task_idx].last_result = Some(result.to_string());
        items[task_idx].execution_count += 1;
        let execution_count = items[task_idx].execution_count;

        self.save(&items)?;
        Ok(json!({
            "status": "ok",
            "id": task_id,
            "execution_count": execution_count,
            "last_result": result
        }).to_string())
    }

    async fn get_dependencies(&self, task_id: &str) -> Result<String, ToolError> {
        let items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let task = items
            .iter()
            .find(|i| i.id == task_id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", task_id)))?;

        let task_title = task.title.clone();
        let depends_on = task.depends_on.clone();
        let all_deps_satisfied = task.depends_on.iter().all(|dep_id| {
            items.iter().any(|i| i.id == *dep_id && i.status == "completed")
        });

        let deps: Vec<serde_json::Value> = depends_on.iter().filter_map(|dep_id| {
            items.iter().find(|i| i.id == *dep_id).map(|dep| {
                json!({
                    "id": dep.id,
                    "title": dep.title,
                    "status": dep.status,
                })
            })
        }).collect();

        Ok(json!({
            "task_id": task_id,
            "task_title": task_title,
            "dependencies": deps,
            "all_deps_satisfied": all_deps_satisfied
        }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_backend() -> FileTodoBackend {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hx_todo_test_{ts}.json"));
        FileTodoBackend::new(path)
    }

    fn create_item(
        b: &FileTodoBackend,
        title: &str,
        group_id: Option<&str>,
        group_title: Option<&str>,
        session_id: &str,
        message_id: &str,
    ) -> String {
        // blocks on the async create via a mini runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(b.create(title, None, None, group_id, group_title, Some(session_id), Some(message_id))).unwrap();
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["group_id"].as_str().unwrap().to_string()
    }

    #[test]
    fn same_message_creates_reuse_one_group() {
        // 主人要求：同一轮回复（同 message_id）的多个 create 必须落在
        // 同一 group，前端只渲染一张卡片，而不是每步一张。
        let b = temp_backend();
        let g1 = create_item(&b, "step A", None, None, "s1", "msg-1");
        let g2 = create_item(&b, "step B", None, None, "s1", "msg-1");
        let g3 = create_item(&b, "step C", None, None, "s1", "msg-1");
        assert_eq!(g1, g2, "same-turn creates must share one group");
        assert_eq!(g1, g3, "same-turn creates must share one group");
    }

    #[test]
    fn different_message_gets_fresh_group() {
        let b = temp_backend();
        let g1 = create_item(&b, "turn 1", None, None, "s1", "msg-1");
        let g2 = create_item(&b, "turn 2", None, None, "s1", "msg-2");
        assert_ne!(g1, g2, "a later reply starting a new plan should mint a fresh group");
    }

    #[test]
    fn explicit_group_id_is_respected() {
        let b = temp_backend();
        let g1 = create_item(&b, "attach", Some("plan-9"), None, "s1", "msg-1");
        let g2 = create_item(&b, "also attach", Some("plan-9"), None, "s1", "msg-9");
        assert_eq!(g1, "plan-9");
        assert_eq!(g2, "plan-9", "explicit group_id must always win");
    }

    #[test]
    fn explicit_group_title_starts_new_group() {
        let b = temp_backend();
        let g1 = create_item(&b, "plan A item", None, Some("Plan A"), "s1", "msg-1");
        let g2 = create_item(&b, "plan B item", None, Some("Plan B"), "s1", "msg-2");
        assert_ne!(g1, g2, "an explicit new group_title on a later reply starts a new plan");
    }
}
