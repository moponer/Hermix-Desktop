//! GroupChat API endpoints.
//!
//! REST endpoints for managing group chat rooms, agents, messages, and tasks.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use crate::HttpServerState;
use hermix_agent::python_alignment::strip_think_blocks_for_ack;
use hermix_gateway::groupchat::persistence::AgentRoomProfile;

/// Resolve the display name for an agent message sender.
/// Prefers the per-room display_name (别名) if set, otherwise
/// falls back to the agent's internal id.
fn msg_sender_name(agent: &AgentRoomProfile) -> String {
    agent.display_name.clone().unwrap_or_else(|| agent.agent_id.clone())
}

// ── Request/Response Types ──────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRoomRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct RoomResponse {
    pub id: String,
    pub name: String,
    pub leader_id: Option<String>,
    pub agent_ids: Vec<String>,
    /// Per-room member profiles (id, role, model_override). The dialog
    /// reads this so it can pre-fill the model select without an extra
    /// round trip per member.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub members: Vec<MemberResponse>,
    /// The built-in leader (总管) virtual identity. Optional so old
    /// rooms predating P1 — which don't have a leader row — still
    /// deserialize cleanly. New rooms always have one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub leader: Option<LeaderResponse>,
    /// Pinned project announcement. `None` when the leader hasn't
    /// published one yet. When present, the banner UI renders this at
    /// the top of the chat surface.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub announcement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub announcement_published_at: Option<String>,
    /// The currently in-flight task, if any. Includes its subtasks so
    /// the panel can render progress without a follow-up round trip.
    /// `None` when there's no active task — typical for rooms that
    /// haven't received a user message since the last task completed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub active_task: Option<TaskResponse>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct MemberResponse {
    pub agent_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
}

/// The built-in leader (项目总管) virtual identity exposed to clients.
///
/// The leader exists once per room and is created automatically on
/// `create_room`. Its `agent_id` is the sentinel
/// [`LEADER_AGENT_ID`] constant; the `session_id` is what the
/// dispatcher binds to so the LeaderOrchestrator prompt can run on
/// a fresh, isolated session for each room.
#[derive(Serialize)]
pub struct LeaderResponse {
    pub agent_id: String,
    pub display_name: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_override: Option<String>,
}

/// Review verdict surfaced to the UI. `None` = not yet reviewed.
/// Matches `task_types::ReviewResult` (the inner layer) so the
/// frontend doesn't have to learn two different shapes.
#[derive(Serialize)]
pub struct ReviewResultResponse {
    pub passed: bool,
    pub comment: String,
    pub reviewer_id: String,
    pub reviewed_at: String,
}

/// A subtask nested under a task. Mirrors `SubTask` but with timestamps
/// pre-formatted as RFC 3339 strings (the persistence layer gives us
/// `DateTime<Utc>`; the API speaks strings).
#[derive(Serialize)]
pub struct SubTaskResponse {
    pub id: String,
    pub task_id: String,
    pub description: String,
    pub assigned_agent_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResultResponse>,
}

#[derive(Serialize)]
pub struct TaskResponse {
    pub id: String,
    pub room_id: String,
    pub leader_agent_id: String,
    pub original_prompt: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub subtasks: Vec<SubTaskResponse>,
}

/// `TaskResponse` builder. Pulls subtasks from a separate query because
/// the persistence trait deliberately keeps them in their own table for
/// pagination and per-subtask updates later on.
fn task_to_response(
    task: &hermix_gateway::groupchat::persistence::GroupTask,
    subtasks: Vec<hermix_gateway::groupchat::persistence::SubTask>,
) -> TaskResponse {
    TaskResponse {
        id: task.id.clone(),
        room_id: task.room_id.clone(),
        leader_agent_id: task.leader_agent_id.clone(),
        original_prompt: task.original_prompt.clone(),
        status: task.status.to_string(),
        plan_json: task.plan_json.clone(),
        final_report: task.final_report.clone(),
        approved_at: task.approved_at.map(|d| d.to_rfc3339()),
        created_at: task.created_at.to_rfc3339(),
        completed_at: task.completed_at.map(|d| d.to_rfc3339()),
        subtasks: subtasks
            .into_iter()
            .map(|s| SubTaskResponse {
                id: s.id,
                task_id: s.task_id,
                description: s.description,
                assigned_agent_id: s.assigned_agent_id,
                status: s.status.to_string(),
                result: s.result,
                started_at: s.started_at.map(|d| d.to_rfc3339()),
                completed_at: s.completed_at.map(|d| d.to_rfc3339()),
                review: s.review.as_ref().map(|r| ReviewResultResponse {
                    passed: r.passed,
                    comment: r.comment.clone(),
                    reviewer_id: r.reviewer_id.clone(),
                    reviewed_at: r.reviewed_at.to_rfc3339(),
                }),
            })
            .collect(),
    }
}

#[derive(Deserialize)]
pub struct AnnouncementRequest {
    /// The new announcement text. Empty string clears the announcement.
    pub text: String,
}

#[derive(Deserialize)]
pub struct UpdateSubtaskRequest {
    /// One of "pending" | "running" | "done" | "failed".
    pub status: String,
    /// Optional result text (typically set when status flips to "done"
    /// or "failed").
    #[serde(default)]
    pub result: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateAgentsRequest {
    pub agents: Vec<AgentRequest>,
}

#[derive(Deserialize)]
pub struct AgentRequest {
    pub agent_id: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct SetLeaderRequest {
    pub agent_id: String,
}

#[derive(Deserialize)]
pub struct UpdateSettingsRequest {
    pub allow_idle_chat: Option<bool>,
    pub idle_interval_secs: Option<u64>,
    pub max_concurrent_agents: Option<u32>,
    pub require_leader_approval: Option<bool>,
}

/// Read-only snapshot of a room's settings. Returned by `GET
/// /api/groupchat/rooms/{room_id}/settings` so the settings drawer
/// can hydrate its form without scanning `list_rooms`.
///
/// Mirrors `RoomSettings` field-for-field — when new settings are
/// added there, add them here too.
#[derive(Serialize)]
pub struct SettingsResponse {
    pub allow_idle_chat: bool,
    pub idle_interval_secs: u64,
    pub max_concurrent_agents: u32,
    pub require_leader_approval: bool,
}

impl From<&hermix_gateway::groupchat::persistence::RoomSettings> for SettingsResponse {
    fn from(s: &hermix_gateway::groupchat::persistence::RoomSettings) -> Self {
        Self {
            allow_idle_chat: s.allow_idle_chat,
            idle_interval_secs: s.idle_interval_secs,
            max_concurrent_agents: s.max_concurrent_agents,
            require_leader_approval: s.require_leader_approval,
        }
    }
}

#[derive(Deserialize)]
pub struct InviteAgentRequest {
    /// Optional role hint — defaults to "member" when omitted.
    /// One of "leader" | "member" | "observer".
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateRulesRequest {
    pub global_constraints: Option<Vec<String>>,
    pub role_permissions: Option<std::collections::HashMap<String, RolePermissionReq>>,
    pub tool_policy_mode: Option<String>,
    pub tool_policy_blacklist: Option<Vec<String>>,
    pub tool_policy_whitelist: Option<Vec<String>>,
    pub penalty_initial_score: Option<i32>,
    pub penalty_reflection_threshold: Option<i32>,
}

#[derive(Deserialize)]
pub struct RolePermissionReq {
    pub denied_tools: Option<Vec<String>>,
    pub can_delegate: Option<bool>,
    pub can_write_memory: Option<bool>,
    pub constraints: Option<String>,
}

#[derive(Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub room_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: String,
    pub created_at: String,
    /// Optional metadata JSON blob. Present on agent messages that had
    /// internal reasoning stripped — the frontend can show a collapsible
    /// "thinking" section from the `raw` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

#[derive(Deserialize)]
pub struct ApproveRequest {
    pub approved: bool,
    pub comment: Option<String>,
}

fn msg_to_response(msg: &hermix_gateway::groupchat::persistence::ChatMessage) -> MessageResponse {
    MessageResponse {
        id: msg.id.clone(),
        room_id: msg.room_id.clone(),
        sender_id: msg.sender_id.clone(),
        sender_name: msg.sender_name.clone(),
        content: msg.content.clone(),
        message_type: msg.message_type.to_string(),
        created_at: msg.created_at.to_rfc3339(),
        metadata: msg.metadata.clone(),
    }
}

/// Extract the leader profile from a list of agents, if present.
///
/// Returns `None` for rooms created before P1 (where the leader row
/// doesn't exist). For new rooms this is always populated because
/// `create_room` auto-inserts the leader.
fn leader_from_agents(agents: &[hermix_gateway::groupchat::persistence::AgentRoomProfile]) -> Option<LeaderResponse> {
    agents
        .iter()
        .find(|a| a.role == hermix_gateway::groupchat::persistence::AgentRole::Leader)
        .map(|a| LeaderResponse {
            agent_id: a.agent_id.clone(),
            display_name: "项目总管".to_string(),
            session_id: a.session_id.clone(),
            model_override: a.model_override.clone(),
        })
}

// ── Handlers ──────────────────────────────────────────────

pub async fn list_rooms(
    State(state): State<HttpServerState>,
) -> Result<Json<Vec<RoomResponse>>, StatusCode> {
    let manager = state.groupchat_manager();
    let rooms = manager.list_rooms().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut responses = Vec::with_capacity(rooms.len());
    for r in rooms {
        let agents = manager.get_room_agents(&r.id).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let members: Vec<MemberResponse> = agents
            .iter()
            .filter(|a| a.agent_id != hermix_gateway::groupchat::persistence::LEADER_AGENT_ID)
            .map(|m| MemberResponse {
                agent_id: m.agent_id.clone(),
                role: m.role.to_string(),
                model_override: m.model_override.clone(),
            })
            .collect();
        let leader = leader_from_agents(&agents);
        let active_task = match manager.get_active_task(&r.id).await {
            Ok(Some(task)) => {
                let subs = manager.get_subtasks(&task.id).await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                Some(task_to_response(&task, subs))
            }
            Ok(None) => None,
            Err(_) => None, // tolerate lookup failure — task is optional metadata
        };
        responses.push(RoomResponse {
            id: r.id,
            name: r.name,
            leader_id: r.leader_id,
            agent_ids: r.agent_ids,
            members,
            leader,
            announcement: r.announcement,
            announcement_published_at: r.announcement_published_at.map(|d| d.to_rfc3339()),
            active_task,
            created_at: r.created_at.to_rfc3339(),
        });
    }
    Ok(Json(responses))
}

pub async fn create_room(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateRoomRequest>,
) -> Result<(StatusCode, Json<RoomResponse>), StatusCode> {
    let manager = state.groupchat_manager();
    let room = manager.create_room(&req.name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Newly created rooms always have the leader row (auto-inserted
    // by the persistence layer), so we can fetch it directly without
    // a separate get_room round trip.
    let agents = manager.get_room_agents(&room.id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let leader = leader_from_agents(&agents);
    Ok((StatusCode::CREATED, Json(RoomResponse {
        id: room.id,
        name: room.name,
        leader_id: room.leader_id,
        agent_ids: room.agent_ids,
        members: Vec::new(),
        leader,
        announcement: None,
        announcement_published_at: None,
        active_task: None,
        created_at: room.created_at.to_rfc3339(),
    })))
}

pub async fn get_room(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<RoomResponse>, StatusCode> {
    let manager = state.groupchat_manager();
    let room = manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let agents = manager.get_room_agents(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let members: Vec<MemberResponse> = agents
        .iter()
        .filter(|a| a.agent_id != hermix_gateway::groupchat::persistence::LEADER_AGENT_ID)
        .map(|m| MemberResponse {
            agent_id: m.agent_id.clone(),
            role: m.role.to_string(),
            model_override: m.model_override.clone(),
        })
        .collect();
    let leader = leader_from_agents(&agents);
    let active_task = match manager.get_active_task(&room_id).await {
        Ok(Some(task)) => {
            let subs = manager.get_subtasks(&task.id).await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Some(task_to_response(&task, subs))
        }
        Ok(None) => None,
        Err(_) => None,
    };
    Ok(Json(RoomResponse {
        id: room.id,
        name: room.name,
        leader_id: room.leader_id,
        agent_ids: room.agent_ids,
        members,
        leader,
        announcement: room.announcement,
        announcement_published_at: room.announcement_published_at.map(|d| d.to_rfc3339()),
        active_task,
        created_at: room.created_at.to_rfc3339(),
    }))
}

pub async fn delete_room(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    // Inline the three steps so the `&Manager` borrow does not
    // span the `idle.stop_room.await` call (parking_lot guards are
    // `!Send`, which would make the future `!Send` and break
    // axum 0.8's Handler bound). Same logic as
    // `GroupChatManager::delete_room` — kept in sync via the
    // `delete_room_inline` helper below.
    manager.delete_room_inline(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_agents(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateAgentsRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();

    // First remove all existing user-invited agents — but NEVER the
    // built-in leader. The leader is permanent; bulk replace must skip it.
    let existing = manager.get_room_agents(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for agent in existing {
        if agent.agent_id == hermix_gateway::groupchat::persistence::LEADER_AGENT_ID {
            continue; // skip leader — it's auto-managed
        }
        let _ = manager.remove_agent_from_room(&room_id, &agent.agent_id).await;
    }

    // Add new agents — also reject any attempt to re-add the leader
    // sentinel via this bulk endpoint.
    for agent_req in &req.agents {
        if agent_req.agent_id == hermix_gateway::groupchat::persistence::LEADER_AGENT_ID {
            continue;
        }
        let role = match agent_req.role.as_str() {
            "leader" => hermix_gateway::groupchat::persistence::AgentRole::Leader,
            "observer" => hermix_gateway::groupchat::persistence::AgentRole::Observer,
            _ => hermix_gateway::groupchat::persistence::AgentRole::Member,
        };
        let _ = manager.add_agent_to_room(&room_id, &agent_req.agent_id, role).await;
    }

    Ok(StatusCode::OK)
}

pub async fn set_leader(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<SetLeaderRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    manager.set_leader(&room_id, &req.agent_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

/// Invite an agent into the room.
///
/// `POST /api/groupchat/rooms/{room_id}/agents/{agent_id}`
///
/// Body: `{ "role": "leader" | "member" | "observer" }` (optional; defaults to member).
/// 404 if the room doesn't exist; 500 on persistence failure.
/// Idempotent for "member" role — re-inviting an already-present member
/// returns 200 without error.  Re-inviting as a different role upgrades
/// the row's role (so promoting member → leader is a single call).
pub async fn invite_agent(
    State(state): State<HttpServerState>,
    Path((room_id, agent_id)): Path<(String, String)>,
    body: Option<Json<InviteAgentRequest>>,
) -> Result<StatusCode, StatusCode> {
    // The built-in leader is a sentinel identity auto-created with
    // every room. Users must never invite it (the room already has it).
    if agent_id == hermix_gateway::groupchat::persistence::LEADER_AGENT_ID {
        return Err(StatusCode::BAD_REQUEST);
    }

    let manager = state.groupchat_manager();

    // Make sure the room exists — fail fast with 404 instead of silently
    // creating a dangling agent row in a non-existent room.
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let role = body
        .as_ref()
        .and_then(|b| b.0.role.as_deref())
        .map(|r| match r {
            "leader" => hermix_gateway::groupchat::persistence::AgentRole::Leader,
            "observer" => hermix_gateway::groupchat::persistence::AgentRole::Observer,
            _ => hermix_gateway::groupchat::persistence::AgentRole::Member,
        })
        .unwrap_or(hermix_gateway::groupchat::persistence::AgentRole::Member);

    manager.add_agent_to_room(&room_id, &agent_id, role).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

/// Remove an agent from the room.
///
/// `DELETE /api/groupchat/rooms/{room_id}/agents/{agent_id}`
///
/// Does **not** delete the agent itself — agents live outside of rooms.
/// 404 if the room doesn't exist; 200 even if the agent wasn't a member
/// (idempotent — kicking a non-member is a no-op).
pub async fn kick_agent(
    State(state): State<HttpServerState>,
    Path((room_id, agent_id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    // The built-in leader is permanently part of every room and cannot
    // be kicked. 400 instead of 404 so the client gets a clear signal
    // ("you tried to do something forbidden"), not "the agent wasn't found".
    if agent_id == hermix_gateway::groupchat::persistence::LEADER_AGENT_ID {
        return Err(StatusCode::BAD_REQUEST);
    }

    let manager = state.groupchat_manager();

    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    manager.remove_agent_from_room(&room_id, &agent_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct UpdateMemberModelRequest {
    /// Model id to override for this agent in this room, or `None`/`null`
    /// to clear the override (revert to the agent's default model).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateMemberDisplayNameRequest {
    /// Display name (别名) for this agent in this room. Pass `null`,
    /// empty string, or whitespace-only to clear the alias and revert
    /// to the agent's global name. The persistence layer normalises
    /// whitespace so the API is forgiving.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Snapshot of one member in a room — used by the dialog's "current
///  members" tab so the UI can render alias + role + model in one
///  round trip without re-fetching the full room.
#[derive(Serialize)]
pub struct MemberSnapshot {
    pub agent_id: String,
    pub role: String,
    pub model_override: Option<String>,
    pub display_name: Option<String>,
    pub session_id: String,
}

/// P3 — flat rank projection so the frontend can render the badge
/// + tier name + score in one round trip. Mirrors the gateway's
/// `RankSnapshot` (no rank internals leak past this boundary).
#[derive(Serialize)]
pub struct RankResponse {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,
    pub tier: String,
    pub tier_cn: String,
    pub badge_emoji: String,
    pub total_tasks: u32,
    pub consecutive_no_violation: u32,
    /// P1.5 — set when the tier changed during the last score adjustment.
    /// `null` when the tier stayed the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_tier: Option<String>,
}

impl From<hermix_gateway::groupchat::persistence::RankSnapshot> for RankResponse {
    fn from(s: hermix_gateway::groupchat::persistence::RankSnapshot) -> Self {
        Self {
            agent_id: s.agent_id,
            room_id: s.room_id,
            score: s.score,
            tier: s.tier,
            tier_cn: s.tier_cn,
            badge_emoji: s.badge_emoji,
            total_tasks: s.total_tasks,
            consecutive_no_violation: s.consecutive_no_violation,
            previous_tier: s.previous_tier,
        }
    }
}

/// `GET /api/groupchat/rooms/{room_id}/members` — list every member
/// of the room (including the built-in `__leader__`) with the
/// per-room overrides that the dialog needs. Returns 404 if the room
/// does not exist. The leader row is included so the UI can render
/// it in the same list shape as user-invited agents.
pub async fn list_room_members(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<Vec<MemberSnapshot>>, StatusCode> {
    let manager = state.groupchat_manager();
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let profiles = manager
        .list_room_members(&room_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        profiles
            .into_iter()
            .map(|p| MemberSnapshot {
                agent_id: p.agent_id,
                role: p.role.to_string(),
                model_override: p.model_override,
                display_name: p.display_name,
                session_id: p.session_id,
            })
            .collect(),
    ))
}

/// `GET /api/groupchat/rooms/{room_id}/ranks` — list rank (勋章)
/// snapshots for every member of the room, sorted by score DESC.
/// Pre-P3 rooms get backfilled with default rank rows on the next
/// `ensure_db` migration, so this endpoint will always return at
/// least the synthetic leader row for any room opened in the
/// desktop app after the upgrade. 404 if the room doesn't exist.
pub async fn list_room_ranks(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<Vec<RankResponse>>, StatusCode> {
    let manager = state.groupchat_manager();
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let ranks = manager
        .list_room_ranks(&room_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ranks.into_iter().map(RankResponse::from).collect()))
}

/// Update the per-room display name (别名) for a single agent.
///
/// `PATCH /api/groupchat/rooms/{room_id}/agents/{agent_id}/display-name`
///
/// Body: `{ "display_name": "首席后端" }` — or `"display_name": null` /
/// empty string to clear the alias.
///
/// Notes:
/// - The built-in leader (`__leader__`) is system-controlled; we accept
///   updates so an operator can give the leader a room-specific title
///   (e.g. "项目总管 · 物流平台") without breaking the leader-driven
///   send_message flow (which dispatches by id, not display name).
/// - 404 if the room or the agent is not a member of the room.
/// - 200 on success. Idempotent.
pub async fn update_member_display_name(
    State(state): State<HttpServerState>,
    Path((room_id, agent_id)): Path<(String, String)>,
    Json(req): Json<UpdateMemberDisplayNameRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    manager
        .set_member_display_name(&room_id, &agent_id, req.display_name.as_deref())
        .await
        .map_err(|e| {
            if e.contains("not in room") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;
    Ok(StatusCode::OK)
}

/// Update the per-room model override for a single agent.
///
/// `PATCH /api/groupchat/rooms/{room_id}/agents/{agent_id}`
///
/// Body: `{ "model": "claude-sonnet-4.5" }` — or `"model": null` to clear.
/// 404 if the room or the agent is not a member of the room.
/// 200 on success. Idempotent.
pub async fn update_member_model(
    State(state): State<HttpServerState>,
    Path((room_id, agent_id)): Path<(String, String)>,
    Json(req): Json<UpdateMemberModelRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();

    // Confirm room exists and agent is a member before touching persistence.
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let model = req.model.as_deref().filter(|s| !s.is_empty());
    manager
        .set_member_model(&room_id, &agent_id, model)
        .await
        .map_err(|e| {
            // The persistence layer returns "agent not in room" if there's
            // no matching row — surface that as 404 to the client.
            if e.contains("not in room") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;
    Ok(StatusCode::OK)
}

pub async fn update_settings(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();

    let current = manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut settings = current.settings;
    if let Some(v) = req.allow_idle_chat { settings.allow_idle_chat = v; }
    if let Some(v) = req.idle_interval_secs { settings.idle_interval_secs = v; }
    if let Some(v) = req.max_concurrent_agents { settings.max_concurrent_agents = v; }
    if let Some(v) = req.require_leader_approval { settings.require_leader_approval = v; }

    manager.update_settings(&room_id, &settings).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

pub async fn get_settings(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<SettingsResponse>, StatusCode> {
    let manager = state.groupchat_manager();
    let room = manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(SettingsResponse::from(&room.settings)))
}

#[derive(Serialize)]
pub struct SendMessageResponse {
    pub user_message: MessageResponse,
    pub agent_messages: Vec<MessageResponse>,
}

pub async fn send_message(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, StatusCode> {
    let manager = state.groupchat_manager();
    let sender_id = req.sender_id.unwrap_or_else(|| "user".to_string());
    let sender_name = req.sender_name.unwrap_or_else(|| "你".to_string());
    let content = req.content.clone();

    // Save user message
    let user_msg = manager.send_message(&room_id, &sender_id, &sender_name, &content).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user_resp = msg_to_response(&user_msg);

    let mut agent_responses = Vec::new();

    // ── Dispatch agents with a 90-second time budget ─────
    // Without a timeout, invalid LLM keys or rate limits could
    // freeze the HTTP response for minutes (exponential-backoff
    // retries in the agent loop).  We isolate the dispatch in a
    // timeout and always reply with whatever we collected (even
    // just the user message + a system hint).
    let room_name = manager
        .get_room(&room_id).await.ok().flatten()
        .map(|r| r.name)
        .unwrap_or_else(|| room_id.clone());
    let agents = manager.get_room_agents(&room_id).await.unwrap_or_default();

    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        run_agent_dispatch(
            &state, &room_id, &content, &room_name,
            &agents, &mut agent_responses,
        ),
    ).await;

    // ── Fleet failure → surface a system message ─────────
    // When every agent dispatch failed silently (no LLM provider,
    // invalid credentials, connection errors, etc.), the frontend
    // would show an empty response — no feedback at all.  Insert a
    // system-level hint so the user knows *why* nobody replied.
    if agent_responses.is_empty() {
        tracing::warn!(room = %room_id, "groupchat send_message: all dispatch failed, inserting system hint");
        let hint = if manager.dispatcher().is_none() {
            "群聊尚未就绪：LLM Provider 未配置。请在设置中配置 API Key 后重试。"
        } else {
            "所有成员暂时无法回复，请检查 LLM Provider 配置或稍后重试。"
        };
        if let Ok(saved) = manager.save_agent_message(
            &room_id,
            "__system__",
            "系统",
            hint,
            hermix_gateway::groupchat::persistence::MessageType::System,
        ).await {
            agent_responses.push(msg_to_response(&saved));
        }
    }

    Ok(Json(SendMessageResponse {
        user_message: user_resp,
        agent_messages: agent_responses,
    }))
}

/// Run agent dispatch for a room (leader analysis + subtasks + aggregation).
///
/// Extracted into its own function so `send_message` can wrap it in a 90-second
/// timeout — without one, flaky LLM providers could freeze the HTTP response for
/// minutes via exponential-backoff retries inside the agent loop.
async fn run_agent_dispatch(
    state: &HttpServerState,
    room_id: &str,
    content: &str,
    _room_name: &str,
    agents: &[hermix_gateway::groupchat::persistence::AgentRoomProfile],
    agent_responses: &mut Vec<MessageResponse>,
) {
    let manager = state.groupchat_manager();
    let leader = agents.iter().find(|a| a.role == hermix_gateway::groupchat::persistence::AgentRole::Leader);

    if let Some(dispatcher) = manager.dispatcher() {
        let dispatcher = &dispatcher;
        let messages = manager.get_messages(room_id, 20).await.unwrap_or_default();
        let context: Vec<String> = messages.iter().rev().take(10).rev()
            .map(|m| format!("{}: {}", m.sender_name, m.content))
            .collect();
        let room_context = context.join("\n");

        if let Some(leader_agent) = leader {
            // ── Leader-driven flow ────────────────────────────────
            // Step 1: Dispatch to leader for analysis + task decomposition
            let leader_prompt = hermix_gateway::groupchat::leader::LeaderOrchestrator::build_leader_prompt(
                agents,
                &agents.iter().map(|a| (a.agent_id.clone(), a.agent_id.clone())).collect::<Vec<_>>(),
            );

            match dispatcher.dispatch_to_agent(
                room_id,
                &leader_agent.agent_id,
                &leader_agent.session_id,
                &format!("{}\n\n房间上下文：\n{}", leader_prompt, room_context),
                content,
                &room_context,
            ).await {
                Ok(leader_reply) => {
                    let leader_reply_raw = leader_reply.clone();
                    let leader_reply = strip_think_blocks_for_ack(&leader_reply);

                    let meta = Some(serde_json::json!({"raw": leader_reply_raw}).to_string());
                    if let Ok(saved) = manager.save_agent_message_with_meta(
                        room_id,
                        &leader_agent.agent_id,
                        &msg_sender_name(leader_agent),
                        &leader_reply,
                        hermix_gateway::groupchat::persistence::MessageType::Chat,
                        meta,
                    ).await {
                        agent_responses.push(msg_to_response(&saved));
                    }

                    // M3 — announcement decision merged into leader prompt.
                    // Parse from the leader reply instead of a separate LLM call.
                    if let Some(announce_text) = hermix_gateway::groupchat::leader::LeaderOrchestrator::parse_announcement_from_reply(&leader_reply) {
                        if let Err(e) = manager.set_announcement(room_id, Some(&announce_text)).await {
                            tracing::warn!(error = %e, "leader auto-publish announcement failed");
                        } else {
                            let mirror = format!("📢 我刚把以下内容发布为本群公告：\n\n{}", announce_text);
                            let _ = manager.save_agent_message(
                                room_id,
                                &leader_agent.agent_id,
                                &msg_sender_name(leader_agent),
                                &mirror,
                                hermix_gateway::groupchat::persistence::MessageType::Chat,
                            ).await;
                        }
                    }

                    // Step 2: Parse task plan
                    let plan = hermix_gateway::groupchat::leader::LeaderOrchestrator::parse_task_plan(&leader_reply);

                    // Step 3: Dispatch subtasks
                    let non_leader_agents: Vec<_> = agents.iter()
                        .filter(|a| a.role != hermix_gateway::groupchat::persistence::AgentRole::Leader)
                        .collect();

                    for subtask in &plan.subtasks {
                        let target_agents = if !subtask.assigned_to.is_empty() {
                            non_leader_agents.iter()
                                .filter(|a| a.agent_id == subtask.assigned_to)
                                .cloned()
                                .collect::<Vec<_>>()
                        } else {
                            non_leader_agents.clone()
                        };

                        for agent in &target_agents {
                            let mut task_msg = subtask.description.clone();
                            if let Some(ref ctx) = subtask.context {
                                task_msg.push_str(&format!("\n\n补充上下文：{}", ctx));
                            }
                            match dispatcher.dispatch_to_agent(
                                room_id,
                                &agent.agent_id,
                                &agent.session_id,
                                &format!("你是{}，在群聊室中。你收到了总管分配的子任务。\n\n房间上下文：\n{}", agent.agent_id, room_context),
                                &task_msg,
                                &room_context,
                            ).await {
                                Ok(response) => {
                                    let meta = Some(serde_json::json!({"raw": &response}).to_string());
                                    if let Ok(saved) = manager.save_agent_message_with_meta(
                                        room_id,
                                        &agent.agent_id,
                                        &msg_sender_name(agent),
                                        &strip_think_blocks_for_ack(&response),
                                        hermix_gateway::groupchat::persistence::MessageType::Chat,
                                        meta,
                                    ).await {
                                        agent_responses.push(msg_to_response(&saved));
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(agent = %agent.agent_id, error = %e, "groupchat subtask dispatch failed");
                                }
                            }
                        }
                    }

                    // Step 4: Leader aggregation
                    let subtask_summaries: Vec<String> = agent_responses.iter()
                        .filter(|m| m.sender_id != leader_agent.agent_id)
                        .map(|m| format!("{}: {}", m.sender_name, m.content))
                        .collect();

                    if !subtask_summaries.is_empty() {
                        let aggregation_prompt = format!(
                            "子任务执行结果如下：\n{}\n\n请验证以上结果，汇总后给出最终结论交给用户。",
                            subtask_summaries.join("\n\n")
                        );
                        match dispatcher.dispatch_to_agent(
                            room_id,
                            &leader_agent.agent_id,
                            &leader_agent.session_id,
                            &format!("{}\n\n房间上下文：\n{}", leader_prompt, room_context),
                            &aggregation_prompt,
                            &room_context,
                        ).await {
                            Ok(final_reply) => {
                                let meta = Some(serde_json::json!({"raw": &final_reply}).to_string());
                                if let Ok(saved) = manager.save_agent_message_with_meta(
                                    room_id,
                                    &leader_agent.agent_id,
                                    &msg_sender_name(leader_agent),
                                    &strip_think_blocks_for_ack(&final_reply),
                                    hermix_gateway::groupchat::persistence::MessageType::Chat,
                                    meta,
                                ).await {
                                    agent_responses.push(msg_to_response(&saved));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "leader aggregation failed");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "leader dispatch failed, falling back to broadcast");
                    broadcast_to_agents(agents, room_id, &room_context, content, dispatcher, &manager, agent_responses).await;
                }
            }
        } else {
            broadcast_to_agents(agents, room_id, &room_context, content, dispatcher, &manager, agent_responses).await;
        }
    }
}

/// Broadcast message to all agents (no leader coordination).
async fn broadcast_to_agents(
    agents: &[hermix_gateway::groupchat::persistence::AgentRoomProfile],
    room_id: &str,
    room_context: &str,
    content: &str,
    dispatcher: &Arc<dyn hermix_gateway::groupchat::dispatcher::AgentDispatcher>,
    manager: &Arc<hermix_gateway::groupchat::GroupChatManager>,
    agent_responses: &mut Vec<MessageResponse>,
) {
    for agent in agents {
        match dispatcher.dispatch_to_agent(
            room_id,
            &agent.agent_id,
            &agent.session_id,
            &format!("你是{}，在群聊室中。\n\n房间上下文：\n{}", agent.agent_id, room_context),
            content,
            room_context,
        ).await {
            Ok(response) => {
                let meta = Some(serde_json::json!({"raw": &response}).to_string());
                if let Ok(saved) = manager.save_agent_message_with_meta(
                    room_id,
                    &agent.agent_id,
                    &msg_sender_name(agent),
                    &strip_think_blocks_for_ack(&response),
                    hermix_gateway::groupchat::persistence::MessageType::Chat,
                    meta,
                ).await {
                    agent_responses.push(msg_to_response(&saved));
                }
            }
            Err(e) => {
                tracing::warn!(agent = %agent.agent_id, error = %e, "groupchat agent dispatch failed");
            }
        }
    }
}

/// `GET /api/groupchat/rooms/{room_id}/messages?before_id=...&limit=50`
///
/// M4 — supports cursor-based pagination. Returns up to `limit` (default 50)
/// messages older than `before_id`. When `before_id` is omitted, returns the
/// most recent messages. Pass the oldest message id from the previous page
/// as `before_id` to load the next page.
pub async fn get_messages(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<MessageQueryParams>,
) -> Result<Json<Vec<MessageResponse>>, StatusCode> {
    let manager = state.groupchat_manager();
    let limit = params.limit.unwrap_or(50).min(200);
    let messages = manager.get_messages_paginated(&room_id, params.before_id.as_deref(), limit as u32).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let responses: Vec<MessageResponse> = messages.into_iter().map(|m| MessageResponse {
        id: m.id,
        room_id: m.room_id,
        sender_id: m.sender_id,
        sender_name: m.sender_name,
        content: m.content,
        message_type: m.message_type.to_string(),
        created_at: m.created_at.to_rfc3339(),
        metadata: m.metadata,
    }).collect();
    Ok(Json(responses))
}

/// M4 — query parameters for paginated message loading.
#[derive(Debug, serde::Deserialize)]
pub struct MessageQueryParams {
    before_id: Option<String>,
    limit: Option<usize>,
}

pub async fn approve(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<ApproveRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    let comment = req.comment.unwrap_or_default();
    let msg = format!("✅ 用户批准了方案{}", if comment.is_empty() { String::new() } else { format!("：{}", comment) });
    let _ = manager.send_message(&room_id, "system", "系统", &msg).await;
    Ok(StatusCode::OK)
}

pub async fn reject(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<ApproveRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    let comment = req.comment.unwrap_or_default();
    let msg = format!("❌ 用户驳回了方案{}", if comment.is_empty() { String::new() } else { format!("：{}", comment) });
    let _ = manager.send_message(&room_id, "system", "系统", &msg).await;
    Ok(StatusCode::OK)
}

pub async fn update_rules(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateRulesRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();

    let mut room = manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(constraints) = req.global_constraints {
        room.rules.global_constraints = constraints;
    }
    if let Some(role_perms) = req.role_permissions {
        for (role, perm) in role_perms {
            let mut permissions = room.rules.role_permissions.get(&role).cloned()
                .unwrap_or_default();
            if let Some(denied) = perm.denied_tools { permissions.denied_tools = denied; }
            if let Some(can_delegate) = perm.can_delegate { permissions.can_delegate = can_delegate; }
            if let Some(can_write) = perm.can_write_memory { permissions.can_write_memory = can_write; }
            if let Some(constraints) = perm.constraints { permissions.constraints = constraints; }
            room.rules.role_permissions.insert(role, permissions);
        }
    }
    if let Some(mode) = req.tool_policy_mode {
        match mode.as_str() {
            "whitelist" => room.rules.tool_policy.mode = hermix_gateway::groupchat::persistence::ToolPolicyMode::Whitelist,
            "strict" => room.rules.tool_policy.mode = hermix_gateway::groupchat::persistence::ToolPolicyMode::Strict,
            _ => room.rules.tool_policy.mode = hermix_gateway::groupchat::persistence::ToolPolicyMode::Blacklist,
        }
    }
    if let Some(blacklist) = req.tool_policy_blacklist { room.rules.tool_policy.blacklisted = blacklist; }
    if let Some(whitelist) = req.tool_policy_whitelist { room.rules.tool_policy.whitelisted = whitelist; }
    if let Some(score) = req.penalty_initial_score { room.rules.penalty_policy.initial_score = score; }
    if let Some(threshold) = req.penalty_reflection_threshold { room.rules.penalty_policy.reflection_threshold = threshold; }

    manager.update_room(&room).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}

// ── Announcement Endpoints (P2) ─────────────────────────────────

/// Publish or replace the room's pinned announcement.
///
/// `POST /api/groupchat/rooms/{room_id}/announcement`
///
/// Body: `{ "text": "本群目标：…" }`. Pass an empty `text` to clear.
/// `published_at` is captured server-side from the manager's clock so
/// clients don't have to send a timestamp. The leader is the only
/// entity that publishes; the API doesn't currently gate this on
/// caller identity because the in-app client is the only consumer —
/// when other callers appear, gate behind an auth check.
pub async fn publish_announcement(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(req): Json<AnnouncementRequest>,
) -> Result<Json<AnnouncementResponse>, StatusCode> {
    let manager = state.groupchat_manager();

    // Fail fast if the room doesn't exist; otherwise the persistence
    // layer would silently no-op.
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let text = req.text.trim().to_string();
    let text_opt = if text.is_empty() { None } else { Some(text.as_str()) };
    manager.set_announcement(&room_id, text_opt).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Re-read so we return the canonical state (incl. published_at).
    let room = manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(AnnouncementResponse {
        announcement: room.announcement,
        announcement_published_at: room.announcement_published_at.map(|d| d.to_rfc3339()),
    }))
}

/// Clear the room's pinned announcement.
///
/// `DELETE /api/groupchat/rooms/{room_id}/announcement`
///
/// Idempotent: returns 200 even when no announcement was set.
pub async fn clear_announcement(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();
    manager.set_announcement(&room_id, None).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

#[derive(Serialize)]
pub struct AnnouncementResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announcement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announcement_published_at: Option<String>,
}

// ── Task Endpoints (P2) ─────────────────────────────────────────

/// Get the active task (with its subtasks) for a room.
///
/// `GET /api/groupchat/rooms/{room_id}/tasks/active`
///
/// Returns 200 with `{ task: null }` (or 200 with empty task fields)
/// when no active task exists. We use 200 + null rather than 404 so
/// the UI can poll cheaply without handling a special "no task" state.
pub async fn get_active_task(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<ActiveTaskResponse>, StatusCode> {
    let manager = state.groupchat_manager();

    // Confirm room exists — list_rooms / get_room will populate the
    // active_task in their payload, but this dedicated endpoint is
    // used for polling after a leader dispatch, so a missing room
    // should return 404.
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let task = manager.get_active_task(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = match task {
        Some(t) => {
            let subs = manager.get_subtasks(&t.id).await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            ActiveTaskResponse {
                task: Some(task_to_response(&t, subs)),
            }
        }
        None => ActiveTaskResponse { task: None },
    };
    Ok(Json(response))
}

#[derive(Serialize)]
pub struct ActiveTaskResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskResponse>,
}


/// List all tasks for a room, newest first. Powers the history view
/// inside the task panel drawer.
///
/// `GET /api/groupchat/rooms/{room_id}/tasks`
pub async fn list_room_tasks(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
) -> Result<Json<Vec<TaskResponse>>, StatusCode> {
    let manager = state.groupchat_manager();
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let tasks = manager.list_tasks_for_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks {
        let subs = manager.get_subtasks(&t.id).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        out.push(task_to_response(&t, subs));
    }
    Ok(Json(out))
}

/// Update an existing subtask. Used by the panel to flip status to
/// "running" / "done" / "failed" and store the result text.
///
/// `PATCH /api/groupchat/subtasks/{subtask_id}`
pub async fn update_subtask(
    State(state): State<HttpServerState>,
    Path(subtask_id): Path<String>,
    Json(req): Json<UpdateSubtaskRequest>,
) -> Result<StatusCode, StatusCode> {
    let manager = state.groupchat_manager();

    // Find the subtask across all rooms. O(n) but task counts per
    // room are small (<10), so this is fine. When this becomes a
    // bottleneck, add a `get_subtask(id)` method to the persistence
    // trait so we can look up directly by primary key. We also
    // capture the owning `room_id` here so the rank hook below has
    // the (room, agent) tuple it needs.
    let rooms = manager.list_rooms().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut existing: Option<hermix_gateway::groupchat::persistence::SubTask> = None;
    let mut owning_room_id: Option<String> = None;
    for r in &rooms {
        let tasks = manager.list_tasks_for_room(&r.id).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        for t in tasks {
            let subs = manager.get_subtasks(&t.id).await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if let Some(s) = subs.into_iter().find(|s| s.id == subtask_id) {
                existing = Some(s);
                owning_room_id = Some(r.id.clone());
                break;
            }
        }
        if existing.is_some() { break; }
    }

    let mut updated = match existing {
        Some(s) => s,
        None => return Err(StatusCode::NOT_FOUND),
    };

    // Capture the pre-mutation status so we can detect transitions
    // for the rank hook after the write commits.
    let old_status = updated.status;

    updated.status = req.status.parse()
        .unwrap_or(hermix_gateway::groupchat::persistence::SubTaskStatus::Pending);
    if let Some(r) = req.result { updated.result = Some(r); }
    // Stamp started_at on the running transition (only first time).
    if updated.status == hermix_gateway::groupchat::persistence::SubTaskStatus::Running
        && updated.started_at.is_none()
    {
        updated.started_at = Some(chrono::Utc::now());
    }
    // Stamp completed_at on terminal transitions.
    if matches!(
        updated.status,
        hermix_gateway::groupchat::persistence::SubTaskStatus::Done
            | hermix_gateway::groupchat::persistence::SubTaskStatus::Failed
    ) {
        updated.completed_at = Some(chrono::Utc::now());
    }
    manager.update_subtask(&updated).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // ── P3 rank hook ────────────────────────────────────────
    // Subtask status transitions move the assigned agent's rank:
    //   any → Done    +10   (reward; bumps total_tasks + streak)
    //   any → Failed  -15   (penalty; resets consecutive_no_violation)
    // Running / Pending transitions are no-ops. The reward / penalty
    // mapping lives at the API edge on purpose — the persistence
    // trait stays free of side-effect policy, and `update_subtask`
    // doesn't have to grow an extra "policy" channel.
    if old_status != updated.status {
        use hermix_gateway::groupchat::persistence::{LEADER_AGENT_ID, SubTaskStatus};
        // Don't score the virtual leader (it owns the plan, not a
        // real subtask), and skip rows that haven't been assigned
        // yet (legacy / cancelled).
        if !updated.assigned_agent_id.is_empty()
            && updated.assigned_agent_id != LEADER_AGENT_ID
        {
            let delta: i32 = match updated.status {
                SubTaskStatus::Done => 10,
                SubTaskStatus::Failed => -15,
                _ => 0,
            };
            if delta != 0 {
                if let Some(rid) = owning_room_id.as_deref() {
                    if let Err(e) = manager.adjust_member_score(
                        rid,
                        &updated.assigned_agent_id,
                        delta,
                    ).await {
                        // Non-fatal: rank write failure mustn't break
                        // the subtask update. Log and move on so the
                        // task panel keeps working.
                        tracing::warn!(
                            "update_subtask rank hook failed for room={} agent={} delta={}: {}",
                            rid, updated.assigned_agent_id, delta, e
                        );
                    }
                }
            }
        }
    }

    Ok(StatusCode::OK)
}

/// P1.4 — cast a vote on a fellow member (+20 up, -20 down).
///
/// `POST /api/groupchat/rooms/{room_id}/vote`
///
/// Body: `{ "target_agent_id": "...", "direction": "up|down" }`
///
/// Returns 200 + the updated rank snapshot on success.
/// 400 if the body is malformed, 404 if the room doesn't exist.
pub async fn vote(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(body): Json<VoteRequest>,
) -> Result<Json<RankResponse>, StatusCode> {
    let delta: i32 = match body.direction {
        VoteKind::Up => 20,
        VoteKind::Down => -20,
    };
    let manager = state.groupchat_manager();
    // verify room exists
    manager.get_room(&room_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let snapshot = manager
        .adjust_member_score(&room_id, &body.target_agent_id, delta)
        .await
        .map_err(|e| {
            tracing::warn!("vote: adjust_member_score failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(RankResponse::from(snapshot)))
}

/// P1.4 request body.
#[derive(Debug, serde::Deserialize)]
pub struct VoteRequest {
    pub target_agent_id: String,
    pub direction: VoteKind,
}

/// P1.4 vote direction. Mirrors `hermix_transport::VoteKind` but
/// lives in the REST layer so the handler doesn't depend on the
/// WS transport crate for a simple enum.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteKind {
    Up,
    Down,
}

/// P4 — dispute a violation record. `POST /api/groupchat/rooms/{room_id}/dispute`
/// Body: `{ "violation_id": "uuid", "reason": "..." }`
/// Returns 200 on success, 404 if violation not found or already terminal.
pub async fn dispute_violation(
    State(state): State<HttpServerState>,
    Path(_room_id): Path<String>,
    Json(body): Json<DisputeRequest>,
) -> Result<StatusCode, StatusCode> {
    let te = state.task_engine.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if te.dispute_violation(&body.violation_id, &body.reason).is_some() {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// P4 — overturn a disputed violation. `POST /api/groupchat/rooms/{room_id}/overturn`
/// Body: `{ "violation_id": "uuid", "agent_id": "...", "comment": "..." }`
/// Returns 200 + restored deduction on success, 404 if not overturnable.
pub async fn overturn_violation(
    State(state): State<HttpServerState>,
    Path(room_id): Path<String>,
    Json(body): Json<OverturnRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let te = state.task_engine.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    te.overturn_violation(&room_id, &body.violation_id, &body.agent_id, "leader", &body.comment)
        .await
        .map(|deduction| Json(serde_json::json!({ "restored": deduction })))
        .map_err(|_| StatusCode::NOT_FOUND)
}

#[derive(Debug, serde::Deserialize)]
pub struct DisputeRequest {
    pub violation_id: String,
    pub reason: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct OverturnRequest {
    pub violation_id: String,
    pub agent_id: String,
    pub comment: String,
}
