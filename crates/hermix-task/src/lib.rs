//! # hermix-task
//!
//! Task domain layer — first-class work units for the orchestration engine.
//!
//! A `Task` is the top-level work unit carrying:
//! - **workspace** — the directory the agent operates in
//! - **conversation** — the session ID linking to `hermix-agent`'s conversation history
//! - **context** — `@`-references, screenshots, attachments, notes
//! - **results** — deliverable artifacts produced by the agent
//!
//! The module provides:
//! - [`Task`] — domain struct
//! - [`TaskRepository`] — SQLite-backed CRUD (shares `sessions.db`, 铁律 D/WAL)
//! - [`TaskService`] — lifecycle management with state-machine validation
//! - [`context`] — `@`-reference parser
//! - [`results`] — `TaskResultRef` collection helpers
//!
//! (Phase 1 — P1-1 through P1-4)

pub mod context;
pub mod repository;
pub mod results;
pub mod service;

// Re-export primary types
pub use context::parse_at_references;
pub use repository::{TaskRepository, TaskRepositoryError};
pub use results::TaskResults;
pub use service::{CreateTaskParams, TaskService, TaskServiceError};

use hermix_core::{TaskContext, TaskMode, TaskResultRef, TaskStatus};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Task domain struct
// ---------------------------------------------------------------------------

/// A first-class work unit in the orchestration engine.
/// /// Maps to the `tasks` table in `sessions.db`. Created by the user
/// (via HTTP API or the frontend task creation flow), processed by
/// the agent loop, and tracked through its lifecycle via
/// [`TaskStatus`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Workspace directory path the agent operates in.
    pub workspace_path: String,
    /// Working mode — determines lifecycle transitions.
    pub mode: TaskMode,
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Parent task ID for multi-agent subtask trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// Context — `@`-references, screenshots, attachments, notes.
    #[serde(default)]
    pub context: TaskContext,
    /// Deliverable artifact references.
    #[serde(default)]
    pub results: Vec<TaskResultRef>,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: i64,
    /// Last update timestamp (Unix milliseconds).
    pub updated_at: i64,
}

impl Task {
    /// Create a new Task with sensible defaults.
    pub fn new(
        title: impl Into<String>,
        workspace_path: impl Into<String>,
        mode: TaskMode,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            workspace_path: workspace_path.into(),
            mode,
            status: TaskStatus::Pending,
            parent_task_id: None,
            context: TaskContext::default(),
            results: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Returns `true` if the task is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// Returns `true` if the task is currently being actively worked on.
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_new_defaults() {
        let task = Task::new("Test Task", "/workspace", TaskMode::Craft);
        assert_eq!(task.title, "Test Task");
        assert_eq!(task.workspace_path, "/workspace");
        assert_eq!(task.mode, TaskMode::Craft);
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.parent_task_id.is_none());
        assert!(task.context.references.is_empty());
        assert!(task.results.is_empty());
        assert!(!task.is_terminal());
        assert!(!task.is_active());
    }

    #[test]
    fn test_task_serde_roundtrip() {
        let task = Task::new("Serialize Me", "/path", TaskMode::Plan);
        let json = serde_json::to_string(&task).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task, back);
    }
}
