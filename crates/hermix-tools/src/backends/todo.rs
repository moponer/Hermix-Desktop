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
    ) -> Result<String, ToolError> {
        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
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
        };
        items.push(item.clone());
        self.save(&items)?;
        Ok(json!({"status": "ok", "id": id, "title": title}).to_string())
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
                })
            })
            .collect();
        Ok(json!({"todos": result, "total": result.len()}).to_string())
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
