use std::collections::HashMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data Models ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupChatRoom {
    pub id: String,
    pub name: String,
    pub leader_id: Option<String>,
    pub agent_ids: Vec<String>,
    pub settings: RoomSettings,
    pub rules: RoomRules,
    /// Project announcement pinned by the leader (system built-in). When
    /// `Some`, the announcement banner is shown at the top of the room
    /// and surfaced as the "current goal" for any new joiner. `None`
    /// means no announcement is currently active. The leader is the only
    /// entity that publishes / clears the announcement — members cannot
    /// edit it directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement_published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Sentinel agent_id for the virtual leader (总管) that every room
/// auto-creates on creation. The leader is a built-in role — not a
/// user-invited agent — but it still occupies a row in
/// `groupchat_room_agents` so the existing send_message leader-driven
/// flow can find and dispatch to it without any code changes.
///
/// Frontend treats this id as a magic string and renders the leader
/// distinctly (👑, centered message bubble, no kick/model controls).
/// API endpoints that mutate per-agent state (invite / kick /
/// update_member_model) reject this id with 400 to prevent the user
/// from accidentally removing or configuring the built-in leader.
///
/// IMPORTANT: this id MUST NOT appear in `room.agent_ids` — only in
/// the `groupchat_room_agents` table's `agent_id` column for the leader
/// row. The room's `agent_ids` list is the user-facing member roster.
pub const LEADER_AGENT_ID: &str = "__leader__";

// ── Session ID generation (groupchat module) ────────────
//
// Replaces the previous `format!("groupchat-leader-{room_id}")` and
// `format!("groupchat-{room_id}-{agent_id}")` synthetic IDs that were
// created at room / agent add time but never inserted into the
// canonical `sessions` table. AgentService then received a session_id
// that resolved to nothing in `session_search` / `memory_prefetch`.
//
// Now we mint plain `Uuid::new_v4()` IDs — the same format used by
// `hermix_gateway::session::Session::new()`, `hermix_agent::acp::session`,
// and every other session-creating path in this workspace. Centralised
// here so the four call sites (room creation, member add, plus their
// in-memory twin) stay in lock-step.
pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSettings {
    pub allow_idle_chat: bool,
    pub idle_interval_secs: u64,
    pub max_concurrent_agents: u32,
    pub require_leader_approval: bool,
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            allow_idle_chat: false,
            idle_interval_secs: 300,
            max_concurrent_agents: 5,
            require_leader_approval: true,
        }
    }
}

// ── Rules ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoomRules {
    pub global_constraints: Vec<String>,
    pub role_permissions: HashMap<String, RolePermissions>,
    pub agent_overrides: HashMap<String, AgentOverride>,
    pub tool_policy: ToolPolicy,
    pub penalty_policy: PenaltyPolicy,
}

impl RoomRules {
    /// Compute effective constraints for an agent by merging all rule layers.
    pub fn effective_constraints(&self, agent_id: &str, role: &AgentRole) -> Vec<String> {
        let mut constraints = Vec::new();

        // Layer 1: Built-in safety rules (cannot be overridden)
        constraints.extend(BUILTIN_SAFETY_RULES.iter().map(|s| s.to_string()));

        // Layer 2: Role built-in tool restrictions
        for tool in builtin_role_denied_tools(role) {
            constraints.push(format!("禁止使用 {} 工具", tool));
        }
        constraints.push(builtin_role_constraints(role).to_string());

        // Layer 3: Room global constraints (user-configured)
        constraints.extend(self.global_constraints.clone());

        // Layer 4: Agent-specific overrides
        if let Some(agent_ov) = self.agent_overrides.get(agent_id) {
            for tool in &agent_ov.extra_denied {
                constraints.push(format!("禁止使用 {} 工具", tool));
            }
            constraints.extend(agent_ov.extra_constraints.clone());
        }

        constraints
    }

    /// Compute effective denied tools for an agent.
    pub fn effective_denied_tools(&self, agent_id: &str, role: &AgentRole) -> Vec<String> {
        let mut denied: Vec<String> = builtin_role_denied_tools(role)
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        if let Some(perms) = self.role_permissions.get(&role.to_string()) {
            denied.extend(perms.denied_tools.clone());
        }

        if let Some(agent_ov) = self.agent_overrides.get(agent_id) {
            denied.extend(agent_ov.extra_denied.clone());
        }

        match self.tool_policy.mode {
            ToolPolicyMode::Blacklist => {
                denied.extend(self.tool_policy.blacklisted.clone());
            }
            ToolPolicyMode::Whitelist => {
                // In whitelist mode, everything NOT in whitelist is denied
                // This is handled at check time, not here
            }
            ToolPolicyMode::Strict => {
                denied.extend(BUILTIN_SAFETY_RULES.iter().map(|s| s.to_string()));
            }
        }

        denied.sort();
        denied.dedup();
        denied
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolePermissions {
    pub denied_tools: Vec<String>,
    pub can_delegate: bool,
    pub can_write_memory: bool,
    pub constraints: String,
}

impl Default for RolePermissions {
    fn default() -> Self {
        Self {
            denied_tools: Vec::new(),
            can_delegate: true,
            can_write_memory: true,
            constraints: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOverride {
    pub extra_denied: Vec<String>,
    pub extra_constraints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub mode: ToolPolicyMode,
    pub blacklisted: Vec<String>,
    pub whitelisted: Vec<String>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            mode: ToolPolicyMode::Blacklist,
            blacklisted: Vec::new(),
            whitelisted: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyMode {
    Blacklist,
    Whitelist,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PenaltyPolicy {
    pub initial_score: i32,
    pub reflection_threshold: i32,
    pub can_fire: bool,
    pub auto_rebuild: bool,
}

impl Default for PenaltyPolicy {
    fn default() -> Self {
        Self {
            initial_score: 10,
            reflection_threshold: 5,
            can_fire: true,
            auto_rebuild: true,
        }
    }
}

// ── Built-in Rules ──────────────────────────────────────────

/// Safety rules that cannot be overridden.
pub const BUILTIN_SAFETY_RULES: &[&str] = &[
    "禁止执行 rm -rf / 或等效的破坏性命令",
    "禁止格式化磁盘或分区",
    "禁止修改系统关键文件（/etc/passwd 等）",
    "禁止执行未授权的网络扫描",
    "禁止访问其他用户的数据目录",
];

/// Role-specific built-in tool restrictions.
pub fn builtin_role_denied_tools(role: &AgentRole) -> Vec<&'static str> {
    match role {
        AgentRole::Leader => vec!["terminal", "file_write", "code_execution"],
        AgentRole::Member => vec!["delegation"],
        AgentRole::Observer => vec![],
    }
}

/// Built-in role constraints text.
pub fn builtin_role_constraints(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::Leader => {
            "你是总管，不能直接执行代码或写文件。你只能通过委派任务给团队成员来完成工作。"
        }
        AgentRole::Member => {
            "你是执行者，不要委派任务。完成后向总管汇报。"
        }
        AgentRole::Observer => {
            "你是观察者，只在被点名时发言。"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Leader,
    Member,
    Observer,
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentRole::Leader => write!(f, "leader"),
            AgentRole::Member => write!(f, "member"),
            AgentRole::Observer => write!(f, "observer"),
        }
    }
}

impl std::str::FromStr for AgentRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "leader" => Ok(AgentRole::Leader),
            "observer" => Ok(AgentRole::Observer),
            _ => Ok(AgentRole::Member),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRoomProfile {
    pub agent_id: String,
    pub room_id: String,
    pub role: AgentRole,
    pub session_id: String,
    /// Per-room model override. `None` means "use the agent's default
    /// model". The room layer is the right place to override because
    /// it's per-discussion (not per-agent globally), so a single agent
    /// can speak with Sonnet in "Code Review" and Haiku in "Brainstorm".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    /// Per-room display name (别名). `None` means "use the agent's
    /// global name". Lets the user rename a specialist just for this
    /// room — e.g. promote "高级开发工程师" to "首席后端" inside a
    /// "项目重构" chat, without touching the agent's catalog entry.
    /// Empty string is treated as "clear alias" by the API layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Flat rank projection returned by `list_room_ranks` / `adjust_member_score`.
/// Decoupled from `rank::AgentRank` so the persistence trait doesn't
/// expose the gateway's internal types to implementors. The `tier` is
/// the same name string the rank module emits (`Bronze`, `Silver`, …)
/// so the frontend can render the badge via a stable lookup table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankSnapshot {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,
    pub tier: String,
    pub tier_cn: String,
    pub badge_emoji: String,
    pub total_tasks: u32,
    pub consecutive_no_violation: u32,
    /// P1.2 — current consecutive-pass streak. 0 if the most recent
    /// verdict was a fail. Resets to 0 on fail, increments by 1 on
    /// pass, capped only by the size of `u32`.
    pub consecutive_passes: u32,
    /// P1.2 — highest `consecutive_passes` ever observed for this
    /// agent in this room. Never decreases. Surfaced in the rank
    /// card for bragging rights.
    pub max_streak: u32,
    /// P1.5 — the tier before the last `refresh_tier()` call, if the
    /// tier actually changed (升段 or 掉段). Transient — never persisted
    /// to SQLite; serialised so the frontend can show a transition toast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_tier: Option<String>,
}

impl RankSnapshot {
    /// Default snapshot at score = 10 (青铜). Used by both `create_room`
    /// (leader row) and `add_agent` (member row) so every member starts
    /// with the same baseline. The tier strings come from
    /// `hermix_gateway::groupchat::rank::RankTier` so the rest of the
    /// stack speaks a single vocabulary.
    pub fn default_for(room_id: impl Into<String>, agent_id: impl Into<String>) -> Self {
        use crate::groupchat::rank::RankTier;
        let tier = RankTier::Bronze;
        Self {
            agent_id: agent_id.into(),
            room_id: room_id.into(),
            score: 10,
            tier: format!("{:?}", tier).to_lowercase(),
            tier_cn: tier.name().to_string(),
            badge_emoji: tier.badge().to_string(),
            total_tasks: 0,
            consecutive_no_violation: 0,
            consecutive_passes: 0,
            max_streak: 0,
            previous_tier: None,
        }
    }

    /// Recompute `tier` / `tier_cn` / `badge_emoji` from `score`. Call
    /// this after mutating `score` so the persisted snapshot stays
    /// consistent (otherwise the badge would drift from the score).
    ///
    /// P1.5 — returns the result of the transition detection so the
    /// caller can decide whether to emit a 升段/掉段 event. `NoTransition`
    /// when the tier hasn't changed.
    pub fn refresh_tier(&mut self) -> TierTransition {
        use crate::groupchat::rank::RankTier;
        let old_tier = self.tier.clone();
        let tier = RankTier::from_score(self.score);
        self.tier = format!("{:?}", tier).to_lowercase();
        self.tier_cn = tier.name().to_string();
        self.badge_emoji = tier.badge().to_string();
        if self.tier != old_tier {
            self.previous_tier = Some(old_tier);
            TierTransition::Changed {
                from: self.previous_tier.clone().unwrap_or_default(),
                to: self.tier.clone(),
            }
        } else {
            TierTransition::NoTransition
        }
    }
}

/// P1.5 — result of a tier re-evaluation after a score adjustment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TierTransition {
    NoTransition,
    Changed {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub room_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: MessageType,
    pub metadata: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Chat,
    Task,
    Result,
    Approval,
    System,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageType::Chat => write!(f, "chat"),
            MessageType::Task => write!(f, "task"),
            MessageType::Result => write!(f, "result"),
            MessageType::Approval => write!(f, "approval"),
            MessageType::System => write!(f, "system"),
        }
    }
}

impl std::str::FromStr for MessageType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "task" => Ok(MessageType::Task),
            "result" => Ok(MessageType::Result),
            "approval" => Ok(MessageType::Approval),
            "system" => Ok(MessageType::System),
            _ => Ok(MessageType::Chat),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupTask {
    pub id: String,
    pub room_id: String,
    pub leader_agent_id: String,
    pub original_prompt: String,
    pub plan_json: Option<String>,
    pub status: TaskStatus,
    pub final_report: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Decomposing,
    Executing,
    Aggregating,
    AwaitingApproval,
    Approved,
    Completed,
    Failed,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::Decomposing => write!(f, "decomposing"),
            TaskStatus::Executing => write!(f, "executing"),
            TaskStatus::Aggregating => write!(f, "aggregating"),
            TaskStatus::AwaitingApproval => write!(f, "awaiting_approval"),
            TaskStatus::Approved => write!(f, "approved"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "decomposing" => Ok(TaskStatus::Decomposing),
            "executing" => Ok(TaskStatus::Executing),
            "aggregating" => Ok(TaskStatus::Aggregating),
            "awaiting_approval" => Ok(TaskStatus::AwaitingApproval),
            "approved" => Ok(TaskStatus::Approved),
            "completed" => Ok(TaskStatus::Completed),
            "failed" => Ok(TaskStatus::Failed),
            _ => Ok(TaskStatus::Pending),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    pub id: String,
    pub task_id: String,
    pub description: String,
    pub assigned_agent_id: String,
    pub session_id: String,
    pub status: SubTaskStatus,
    pub result: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Review verdict (passed/failed + comment) issued by the leader
    /// or a human reviewer. `None` = not yet reviewed. The agent on
    /// revision reads this on re-dispatch so it knows what to fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<super::task_types::ReviewResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubTaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl std::fmt::Display for SubTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubTaskStatus::Pending => write!(f, "pending"),
            SubTaskStatus::Running => write!(f, "running"),
            SubTaskStatus::Done => write!(f, "done"),
            SubTaskStatus::Failed => write!(f, "failed"),
        }
    }
}

impl std::str::FromStr for SubTaskStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "running" => Ok(SubTaskStatus::Running),
            "done" => Ok(SubTaskStatus::Done),
            "failed" => Ok(SubTaskStatus::Failed),
            _ => Ok(SubTaskStatus::Pending),
        }
    }
}

// ── Persistence Trait ─────────────────────────────────────

pub trait RoomPersistence: Send + Sync {
    fn create_room(&self, name: &str) -> Result<GroupChatRoom, String>;
    fn get_room(&self, id: &str) -> Result<Option<GroupChatRoom>, String>;
    fn list_rooms(&self) -> Result<Vec<GroupChatRoom>, String>;
    fn update_room(&self, room: &GroupChatRoom) -> Result<(), String>;
    fn delete_room(&self, id: &str) -> Result<(), String>;
    fn add_agent(&self, room_id: &str, agent_id: &str, role: AgentRole) -> Result<AgentRoomProfile, String>;
    fn remove_agent(&self, room_id: &str, agent_id: &str) -> Result<(), String>;
    fn get_agents(&self, room_id: &str) -> Result<Vec<AgentRoomProfile>, String>;
    fn set_leader(&self, room_id: &str, agent_id: &str) -> Result<(), String>;
    /// Set the per-room model override for a single agent.
    /// Pass `None` to clear the override (revert to agent's default).
    /// Idempotent: setting the same value twice is a no-op.
    fn set_member_model(
        &self,
        room_id: &str,
        agent_id: &str,
        model: Option<&str>,
    ) -> Result<(), String>;
    /// Set the per-room display name (别名) for a single agent.
    /// Pass `None` to clear the alias (revert to agent's global name).
    /// Trims whitespace; empty string is treated as `None` for
    /// idempotency (so toggling "save with blank name" doesn't
    /// accidentally create empty aliases).
    fn set_member_display_name(
        &self,
        room_id: &str,
        agent_id: &str,
        display_name: Option<&str>,
    ) -> Result<(), String>;
    /// Get the rank (勋章) record for every member of a room. Returns
    /// an empty Vec if the room has no rank rows yet (e.g. a freshly
    /// migrated database where ensure_db hasn't backfilled). Each
    /// `RankSnapshot` is a flat projection of `rank::AgentRank` so
    /// the persistence layer doesn't leak the gateway's internal
    /// types through the trait.
    fn list_room_ranks(&self, room_id: &str) -> Result<Vec<RankSnapshot>, String>;
    /// Adjust a member's score by `delta` (positive = reward, negative
    /// = penalty). Creates the row at the default score (10) if it
    /// doesn't exist yet. Returns the post-update snapshot so the
    /// caller doesn't need a second round trip.
    fn adjust_member_score(
        &self,
        room_id: &str,
        agent_id: &str,
        delta: i32,
    ) -> Result<RankSnapshot, String>;
    /// Read an agent's current score in a room. Returns `Ok(None)` if
    /// the agent has no rank row yet.
    fn get_agent_score(&self, room_id: &str, agent_id: &str) -> Result<Option<i32>, String>;
    /// Force-set an agent's score (Yanluo audit path). Values < 0 are
    /// clamped to 0 by the implementation.
    fn set_agent_score(&self, room_id: &str, agent_id: &str, new_score: i32) -> Result<(), String>;
    /// P1.2 — bump or reset the agent's consecutive-pass streak
    /// (设计 §一). Pass: streak += 1, max_streak = max(max, streak).
    /// Fail: streak = 0. Returns the new streak value so the caller
    /// can decide whether to award a milestone bonus without a
    /// second round-trip. Idempotent on missing rows — creates at
    /// default (streak=0) first.
    fn bump_streak(
        &self,
        room_id: &str,
        agent_id: &str,
        passed: bool,
    ) -> Result<u32, String>;
    fn update_settings(&self, room_id: &str, settings: &RoomSettings) -> Result<(), String>;
    fn save_message(&self, msg: &ChatMessage) -> Result<(), String>;
    fn get_messages(&self, room_id: &str, limit: u32) -> Result<Vec<ChatMessage>, String>;
    /// M4 — paginated message loading. Returns up to `limit` messages
    /// older than `before_id`. When `before_id` is `None`, returns
    /// the most recent messages. Default impl delegates to `get_messages`.
    fn get_messages_paginated(
        &self,
        room_id: &str,
        _before_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ChatMessage>, String> {
        // Default: ignore before_id, just return latest N
        self.get_messages(room_id, limit)
    }
    fn save_task(&self, task: &GroupTask) -> Result<(), String>;
    fn get_task(&self, id: &str) -> Result<Option<GroupTask>, String>;
    /// Get the active task for a room. "Active" = the most recently
    /// created task whose status is one of `Pending`, `Decomposing`,
    /// `Executing`, `Aggregating`, or `AwaitingApproval` — i.e. anything
    /// not yet `Approved`/`Completed`/`Failed`. Returns `Ok(None)` if no
    /// active task exists.
    fn get_active_task_for_room(&self, room_id: &str) -> Result<Option<GroupTask>, String>;
    /// List all tasks for a room, newest first. Used for the history
    /// view in the task panel drawer.
    fn list_tasks_for_room(&self, room_id: &str) -> Result<Vec<GroupTask>, String>;
    fn save_subtask(&self, subtask: &SubTask) -> Result<(), String>;
    fn get_subtasks(&self, task_id: &str) -> Result<Vec<SubTask>, String>;
    /// Update an existing subtask in place by id. Returns
    /// `Err("subtask not found")` if no row matches.
    fn update_subtask(&self, subtask: &SubTask) -> Result<(), String>;
    /// Publish or clear the room's pinned announcement. Pass `Some(text)`
    /// to publish (or replace) the announcement, `None` to clear it. The
    /// publish timestamp is captured by the caller (manager layer) and
    /// passed in alongside so the persistence layer doesn't depend on a
    /// wall clock of its own.
    fn set_announcement(
        &self,
        room_id: &str,
        text: Option<&str>,
        published_at: Option<DateTime<Utc>>,
    ) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_id_returns_uuid_v4_format() {
        // UUID v4 canonical form: 8-4-4-4-12 hex chars separated by
        // hyphens. The canonical session format used by `Session::new()`
        // and every other session-creating path in this workspace.
        let id = new_session_id();
        assert_eq!(id.len(), 36, "expected canonical UUID v4 length");
        assert!(
            Uuid::parse_str(&id).is_ok(),
            "new_session_id must produce a parseable UUID, got: {}",
            id
        );
    }

    #[test]
    fn new_session_id_is_unique_across_calls() {
        // Smoke check: two consecutive calls produce distinct IDs.
        // Catches accidental constant seeding or shared-state bugs.
        assert_ne!(new_session_id(), new_session_id());
    }
}
