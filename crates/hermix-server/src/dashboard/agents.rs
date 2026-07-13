//! Custom agent (persona) persistence API.
//!
//! Stores user-created agent personas as individual JSON files under
//! `$HERMIX_HOME/agents/*.json`. The front-end `AgentsView` component
//! calls these endpoints instead of (or in addition to) localStorage
//! so that agents survive across devices and browser caches.
//!
//! ## File layout
//!
//! ```text
//! $HERMIX_HOME/
//!   agents/
//!     agent-1719500000000.json
//!     agent-1719600000000.json
//! ```
//!
//! Each file contains a single `AgentPersonaJson` object.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{HttpError, HttpServerState};

// ---------------------------------------------------------------------------
// Types — match the front-end `AgentPersona` interface exactly.
// ---------------------------------------------------------------------------

/// The on-disk / over-the-wire representation of a custom agent.
///
/// Fields mirror the TypeScript `AgentPersona` interface in
/// `apps/desktop-tauri/src/components/AgentsView.tsx`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPersonaJson {
    pub id: String,
    pub name: String,
    #[serde(default = "default_avatar")]
    pub avatar: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub soul: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub specialty: String,
    #[serde(default)]
    pub dialog_style: String,
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub personality: Vec<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_color_theme")]
    pub color_theme: String,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default)]
    pub last_used_at: u64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_dialog: Option<SampleDialog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleDialog {
    pub q: String,
    pub a: String,
}

fn default_avatar() -> String {
    "🤖".to_string()
}

fn default_color_theme() -> String {
    "violet".to_string()
}

/// Request body for `POST /api/agents/custom`.
///
/// If `id` is present and an agent file with that id exists, the
/// agent is updated. Otherwise a new agent is created.
#[derive(Debug, Clone, Deserialize)]
pub struct SaveAgentRequest {
    /// Omit to create a new agent (the server assigns an id).
    /// Include to update an existing agent.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default = "default_avatar")]
    pub avatar: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub soul: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub specialty: String,
    #[serde(default)]
    pub dialog_style: String,
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub personality: Vec<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_color_theme")]
    pub color_theme: String,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default)]
    pub last_used_at: u64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub sample_dialog: Option<SampleDialog>,
}

/// Response for `POST /api/agents/custom` — returns the saved agent
/// with its id (useful when the client didn't supply one).
#[derive(Debug, Clone, Serialize)]
pub struct SaveAgentResponse {
    pub ok: bool,
    pub agent: AgentPersonaJson,
}

/// Response for `GET /api/agents/custom`.
#[derive(Debug, Clone, Serialize)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentPersonaJson>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn agents_dir(state: &HttpServerState) -> std::path::PathBuf {
    state.hermix_home.join("agents")
}

fn ensure_agents_dir(state: &HttpServerState) -> Result<std::path::PathBuf, HttpError> {
    let dir = agents_dir(state);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to create agents directory: {e}"),
        })?;
    }
    Ok(dir)
}

fn agent_file_path(state: &HttpServerState, id: &str) -> std::path::PathBuf {
    agents_dir(state).join(format!("{id}.json"))
}

fn read_all_agents(state: &HttpServerState) -> Vec<AgentPersonaJson> {
    let dir = agents_dir(state);
    if !dir.exists() {
        return Vec::new();
    }
    let mut agents = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(agent) = serde_json::from_str::<AgentPersonaJson>(&content) {
                    agents.push(agent);
                }
            }
        }
    }
    agents
}

// ---------------------------------------------------------------------------
// GET /api/agents/custom
// ---------------------------------------------------------------------------

pub async fn list_agents(
    State(state): State<HttpServerState>,
) -> Result<Json<ListAgentsResponse>, HttpError> {
    let agents = read_all_agents(&state);
    Ok(Json(ListAgentsResponse { agents }))
}

// ---------------------------------------------------------------------------
// POST /api/agents/custom
// ---------------------------------------------------------------------------

pub async fn save_agent(
    State(state): State<HttpServerState>,
    Json(req): Json<SaveAgentRequest>,
) -> Result<Json<SaveAgentResponse>, HttpError> {
    let _dir = ensure_agents_dir(&state)?;

    let now = chrono::Utc::now().timestamp_millis() as u64;

    // Determine id: use provided id or generate a new one
    let id = req.id.clone().unwrap_or_else(|| format!("agent-{now}"));

    // Check if updating an existing agent (preserve created_at + usage_count)
    let existing = {
        let path = agent_file_path(&state, &id);
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<AgentPersonaJson>(&s).ok())
        } else {
            None
        }
    };

    let created_at = if let Some(ref ex) = existing {
        ex.created_at
    } else if req.created_at > 0 {
        req.created_at
    } else {
        now
    };

    let usage_count = if let Some(ref ex) = existing {
        // If the request provides usage_count, use it (client may be
        // incrementing). Otherwise preserve the existing value.
        if req.usage_count > 0 {
            req.usage_count
        } else {
            ex.usage_count
        }
    } else {
        req.usage_count
    };

    let last_used_at = if req.last_used_at > 0 {
        req.last_used_at
    } else if let Some(ref ex) = existing {
        ex.last_used_at
    } else {
        now
    };

    let agent = AgentPersonaJson {
        id: id.clone(),
        name: req.name,
        avatar: req.avatar,
        role: req.role,
        identity: req.identity,
        soul: req.soul,
        skills: req.skills,
        specialty: req.specialty,
        dialog_style: req.dialog_style,
        background: req.background,
        personality: req.personality,
        system_prompt: req.system_prompt,
        model: req.model,
        color_theme: req.color_theme,
        usage_count,
        last_used_at,
        created_at,
        updated_at: now,
        sample_dialog: req.sample_dialog,
    };

    let json = serde_json::to_string_pretty(&agent).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize agent: {e}"),
    })?;

    let path = agent_file_path(&state, &id);
    std::fs::write(&path, json).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write agent file: {e}"),
    })?;

    tracing::info!(agent_id = %id, "custom agent saved");

    Ok(Json(SaveAgentResponse { ok: true, agent }))
}

// ---------------------------------------------------------------------------
// DELETE /api/agents/custom/{id}
// ---------------------------------------------------------------------------

pub async fn delete_agent(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<HashMap<&'static str, bool>>, HttpError> {
    let path = agent_file_path(&state, &id);
    if !path.exists() {
        return Err(HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("agent '{id}' not found"),
        });
    }
    std::fs::remove_file(&path).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to delete agent file: {e}"),
    })?;

    tracing::info!(agent_id = %id, "custom agent deleted");

    Ok(Json(HashMap::from([("ok", true)])))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state;

    #[tokio::test]
    async fn save_and_list_and_delete() {
        let state = test_state::test_state_with_temp_home().await;

        // Save
        let req = SaveAgentRequest {
            id: None,
            name: "Test Agent".into(),
            avatar: "🤖".into(),
            role: "Tester".into(),
            identity: "I test things".into(),
            soul: "".into(),
            skills: vec!["testing".into()],
            specialty: "".into(),
            dialog_style: "".into(),
            background: "".into(),
            personality: vec![],
            system_prompt: "You are a test agent".into(),
            model: None,
            color_theme: "violet".into(),
            usage_count: 0,
            last_used_at: 0,
            created_at: 0,
            updated_at: 0,
            sample_dialog: None,
        };
        let resp = save_agent(State(state.clone()), Json(req)).await.unwrap();
        assert!(resp.ok);
        assert!(resp.agent.id.starts_with("agent-"));
        let id = resp.agent.id.clone();

        // List
        let list = list_agents(State(state.clone())).await.unwrap();
        assert_eq!(list.agents.len(), 1);
        assert_eq!(list.agents[0].id, id);
        assert_eq!(list.agents[0].name, "Test Agent");

        // Delete
        let del = delete_agent(State(state.clone()), Path(id.clone())).await.unwrap();
        assert_eq!(del.get("ok"), Some(&true));

        // List again — should be empty
        let list2 = list_agents(State(state)).await.unwrap();
        assert!(list2.agents.is_empty());
    }

    #[tokio::test]
    async fn update_preserves_created_at_and_usage() {
        let state = test_state::test_state_with_temp_home().await;

        // Create
        let req = SaveAgentRequest {
            id: Some("agent-test001".into()),
            name: "Original".into(),
            avatar: "🤖".into(),
            role: "Bot".into(),
            identity: "".into(),
            soul: "".into(),
            skills: vec![],
            specialty: "".into(),
            dialog_style: "".into(),
            background: "".into(),
            personality: vec![],
            system_prompt: "prompt".into(),
            model: None,
            color_theme: "violet".into(),
            usage_count: 0,
            last_used_at: 0,
            created_at: 0,
            updated_at: 0,
            sample_dialog: None,
        };
        let resp = save_agent(State(state.clone()), Json(req)).await.unwrap();
        let original_created = resp.agent.created_at;

        // Update with incremented usage_count
        let req2 = SaveAgentRequest {
            id: Some("agent-test001".into()),
            name: "Updated".into(),
            avatar: "🤖".into(),
            role: "Bot".into(),
            identity: "".into(),
            soul: "".into(),
            skills: vec![],
            specialty: "".into(),
            dialog_style: "".into(),
            background: "".into(),
            personality: vec![],
            system_prompt: "prompt".into(),
            model: None,
            color_theme: "violet".into(),
            usage_count: 5,
            last_used_at: 0,
            created_at: 0,
            updated_at: 0,
            sample_dialog: None,
        };
        let resp2 = save_agent(State(state.clone()), Json(req2)).await.unwrap();
        assert_eq!(resp2.agent.name, "Updated");
        assert_eq!(resp2.agent.created_at, original_created);
        assert_eq!(resp2.agent.usage_count, 5);
    }
}
