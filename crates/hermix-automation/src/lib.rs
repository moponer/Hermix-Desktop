//! # hermix-automation
//!
//! Automation engine with three-table separation (Phase 5):
//!
//! - **`automations`** — definition table: name, prompt, schedule, status
//! - **`automation_runtime_state`** — runtime table: last_run, next_run, run_count
//! - **`automation_runs`** — history table: each execution record
//!
//! This separation follows the WorkBuddy architecture: scheduling logic is
//! decoupled from execution history, enabling independent queries and
//! efficient updates without touching the definition row.
//!
//! (Phase 5 — P5-1 through P5-5)

pub mod repository;
pub mod scheduler;
pub mod service;

pub use repository::{AutomationRepository, AutomationRepositoryError};
pub use scheduler::{AutomationScheduler, TriggerFn};
pub use service::{AutomationService, AutomationServiceError};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Schedule type for an automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleType {
    /// Recurring schedule using a cron expression.
    Recurring,
    /// One-time schedule at a specific timestamp.
    Once,
}

impl std::fmt::Display for ScheduleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recurring => f.write_str("recurring"),
            Self::Once => f.write_str("once"),
        }
    }
}

/// Automation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AutomationStatus {
    Active,
    Paused,
}

impl std::fmt::Display for AutomationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => f.write_str("ACTIVE"),
            Self::Paused => f.write_str("PAUSED"),
        }
    }
}

/// Automation definition — maps to the `automations` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Automation {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The prompt to send to the agent when triggered.
    pub prompt: String,
    /// Schedule type: recurring or once.
    pub schedule_type: ScheduleType,
    /// RRULE/cron string for recurring schedules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// ISO8601 timestamp for one-time schedules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    /// Active or Paused.
    pub status: AutomationStatus,
    /// Valid from (ISO8601). If set, automation won't fire before this date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    /// Valid until (ISO8601). If set, automation won't fire after this date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Working directory paths (JSON array). If set, constrains file tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwds: Option<String>,
    /// Connector IDs (JSON array). Which MCP connectors to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_ids_json: Option<String>,
    /// Expert ID for the agent persona.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expert_id: Option<String>,
    /// Model override (e.g. "openai:gpt-4o").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: i64,
    /// Last update timestamp (Unix milliseconds).
    pub updated_at: i64,
}

/// Runtime state — maps to the `automation_runtime_state` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRuntimeState {
    /// FK to `automations.id`.
    pub automation_id: String,
    /// Last execution timestamp (Unix milliseconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    /// Next scheduled execution (Unix milliseconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// Total number of executions.
    pub run_count: i64,
}

/// Run record — maps to the `automation_runs` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRun {
    /// Unique run ID.
    pub id: String,
    /// FK to `automations.id`.
    pub automation_id: String,
    /// Execution start timestamp (Unix milliseconds).
    pub started_at: i64,
    /// Execution end timestamp (Unix milliseconds). `None` if still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// Execution status: "success", "failed", "timeout", "running".
    pub status: String,
    /// Task ID generated by this run (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Error message (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

/// Parameters for creating a new automation.
#[derive(Debug, Clone)]
pub struct CreateAutomationParams {
    pub name: String,
    pub prompt: String,
    pub schedule_type: ScheduleType,
    pub rrule: Option<String>,
    pub scheduled_at: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub cwds: Option<String>,
    pub connector_ids_json: Option<String>,
    pub expert_id: Option<String>,
    pub model_id: Option<String>,
}

impl Default for CreateAutomationParams {
    fn default() -> Self {
        Self {
            name: "Untitled Automation".to_string(),
            prompt: String::new(),
            schedule_type: ScheduleType::Recurring,
            rrule: None,
            scheduled_at: None,
            valid_from: None,
            valid_until: None,
            cwds: None,
            connector_ids_json: None,
            expert_id: None,
            model_id: None,
        }
    }
}

impl Automation {
    /// Create a new automation with sensible defaults.
    pub fn new(params: CreateAutomationParams) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: params.name,
            prompt: params.prompt,
            schedule_type: params.schedule_type,
            rrule: params.rrule,
            scheduled_at: params.scheduled_at,
            status: AutomationStatus::Active,
            valid_from: params.valid_from,
            valid_until: params.valid_until,
            cwds: params.cwds,
            connector_ids_json: params.connector_ids_json,
            expert_id: params.expert_id,
            model_id: params.model_id,
            created_at: now,
            updated_at: now,
        }
    }

    /// Returns `true` if the automation is active.
    pub fn is_active(&self) -> bool {
        self.status == AutomationStatus::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_automation_new_defaults() {
        let params = CreateAutomationParams {
            name: "Daily Brief".into(),
            prompt: "Summarize today's news".into(),
            schedule_type: ScheduleType::Recurring,
            rrule: Some("0 8 * * *".into()),
            ..Default::default()
        };
        let auto = Automation::new(params);
        assert_eq!(auto.name, "Daily Brief");
        assert_eq!(auto.status, AutomationStatus::Active);
        assert!(auto.is_active());
        assert_eq!(auto.schedule_type, ScheduleType::Recurring);
    }

    #[test]
    fn test_schedule_type_display() {
        assert_eq!(ScheduleType::Recurring.to_string(), "recurring");
        assert_eq!(ScheduleType::Once.to_string(), "once");
    }

    #[test]
    fn test_automation_status_display() {
        assert_eq!(AutomationStatus::Active.to_string(), "ACTIVE");
        assert_eq!(AutomationStatus::Paused.to_string(), "PAUSED");
    }
}
