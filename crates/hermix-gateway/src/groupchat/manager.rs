use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use super::dispatcher::AgentDispatcher;
use super::idle::IdleChatManager;
use super::leader::LeaderOrchestrator;
use super::persistence::*;

/// Runtime state for a room (in-memory, not persisted).
#[derive(Default)]
struct RoomRuntime {
    /// Map of agent_id -> session_id for active AgentLoops.
    agent_sessions: HashMap<String, String>,
    /// Current active task, if any.
    active_task_id: Option<String>,
}

/// Central orchestrator for group chat rooms.
pub struct GroupChatManager {
    db: Arc<dyn RoomPersistence>,
    leader: LeaderOrchestrator,
    /// Dispatcher is behind a Mutex so the manager can be shared via
    /// `Arc` and tests (which build a shared `Arc<Manager>` alongside
    /// a `TaskEngineRuntime`) can still install a dispatcher after
    /// the fact without needing `&mut self`.
    dispatcher: parking_lot::Mutex<Option<Arc<dyn AgentDispatcher>>>,
    /// Idle chat timer manager. Constructed lazily inside `set_dispatcher`
    /// so it can hold a clone of the dispatcher. `None` means idle chat
    /// can't run yet (no dispatcher) — `update_settings` will treat
    /// that as a no-op for the timer side, while still persisting the
    /// user's preference. This avoids forcing the constructor to take
    /// a dispatcher and breaking the existing `new(db)` shape.
    idle: parking_lot::Mutex<Option<Arc<IdleChatManager>>>,
    runtime: RwLock<HashMap<String, RoomRuntime>>,
}

use tokio::sync::RwLock;

impl GroupChatManager {
    pub fn new(db: Arc<dyn RoomPersistence>) -> Self {
        Self {
            db,
            leader: LeaderOrchestrator::new(),
            dispatcher: parking_lot::Mutex::new(None),
            idle: parking_lot::Mutex::new(None),
            runtime: RwLock::new(HashMap::new()),
        }
    }

    /// Set the agent dispatcher for real agent responses.
    ///
    /// Also constructs the idle chat manager that piggy-backs on the
    /// same dispatcher. The two are kept on separate `Arc`s so the
    /// idle manager can spawn long-lived timer tasks without
    /// borrowing `&mut self` afterwards.
    pub fn set_dispatcher(&self, dispatcher: Arc<dyn AgentDispatcher>) {
        *self.dispatcher.lock() = Some(dispatcher.clone());
        *self.idle.lock() = Some(Arc::new(IdleChatManager::new(dispatcher)));
    }

    /// Get the dispatcher reference.
    pub fn dispatcher(&self) -> Option<Arc<dyn AgentDispatcher>> {
        self.dispatcher.lock().clone()
    }

    /// P1.3 — install a hook on the idle chat manager that fires after
    /// every successful dispatcher response. The hook is the place
    /// where the message gets persisted to the room and any side
    /// effects (e.g. `adjust_member_score(+3)`) are applied. No-op if
    /// `set_dispatcher` hasn't been called yet (idle manager absent).
    pub fn install_idle_publish_hook(&self, hook: super::idle::IdlePublishHook) {
        let idle = self.idle.lock().clone();
        if let Some(idle) = idle {
            idle.set_publish_hook(hook);
        } else {
            tracing::warn!(
                "install_idle_publish_hook: idle manager not yet constructed \
                 (set_dispatcher must run first) — hook not installed"
            );
        }
    }

    // ── Room CRUD ──────────────────────────────────────────────

    pub async fn create_room(&self, name: &str) -> Result<GroupChatRoom, String> {
        self.db.create_room(name)
    }

    pub async fn list_rooms(&self) -> Result<Vec<GroupChatRoom>, String> {
        self.db.list_rooms()
    }

    pub async fn get_room(&self, id: &str) -> Result<Option<GroupChatRoom>, String> {
        self.db.get_room(id)
    }

    pub async fn update_room(&self, room: &GroupChatRoom) -> Result<(), String> {
        self.db.update_room(room)
    }

    pub async fn delete_room(&self, id: &str) -> Result<(), String> {
        self.delete_room_inline(id).await
    }

    /// Same logic as `delete_room`, but kept as a separate method
    /// so the axum 0.8 handler can call this directly without the
    /// intermediate method dispatch — see
    /// `crates/hermix-server/src/dashboard/groupchat_api.rs::delete_room`.
    /// The inline form means the parking_lot guards don't span an
    /// `.await` of `idle.stop_room`, which would otherwise make the
    /// future `!Send`.
    pub async fn delete_room_inline(&self, id: &str) -> Result<(), String> {
        // Clone the Arc out under the lock so the parking_lot guard
        // does NOT span the `idle.stop_room(id).await` (guards are
        // `!Send` and would break axum 0.8's Handler future bound).
        // Two explicit statements so the `MutexGuard` temporary from
        // `self.idle.lock()` is dropped before the `.await` point.
        let idle_owned = self.idle.lock().clone();
        if let Some(idle) = idle_owned {
            idle.stop_room(id).await;
        }
        self.runtime.write().await.remove(id);
        self.db.delete_room(id)
    }

    // ── Agent Management ──────────────────────────────────────

    pub async fn add_agent_to_room(
        &self,
        room_id: &str,
        agent_id: &str,
        role: AgentRole,
    ) -> Result<AgentRoomProfile, String> {
        self.db.add_agent(room_id, agent_id, role)
    }

    pub async fn remove_agent_from_room(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        self.db.remove_agent(room_id, agent_id)
    }

    pub async fn get_room_agents(&self, room_id: &str) -> Result<Vec<AgentRoomProfile>, String> {
        self.db.get_agents(room_id)
    }

    pub async fn set_leader(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        self.db.set_leader(room_id, agent_id)
    }

    /// Set the per-room model override for a single agent.
    /// `None` clears the override (revert to agent's default).
    pub async fn set_member_model(
        &self,
        room_id: &str,
        agent_id: &str,
        model: Option<&str>,
    ) -> Result<(), String> {
        self.db.set_member_model(room_id, agent_id, model)
    }

    /// Set the per-room display name (别名) for a single agent.
    /// `None` (or empty/whitespace) clears the alias and the
    /// member will fall back to the agent's global name.
    pub async fn set_member_display_name(
        &self,
        room_id: &str,
        agent_id: &str,
        display_name: Option<&str>,
    ) -> Result<(), String> {
        self.db.set_member_display_name(room_id, agent_id, display_name)
    }

    /// Snapshot every member of the room — used by the frontend's
    /// room-member dialog so it can render alias + role + model
    /// overrides without re-fetching the full room. Includes the
    /// built-in `__leader__` row so the UI can render the leader in
    /// the same shape as user-invited agents.
    pub async fn list_room_members(
        &self,
        room_id: &str,
    ) -> Result<Vec<AgentRoomProfile>, String> {
        self.db.get_agents(room_id)
    }

    /// P3 — list rank snapshots for every member of a room, sorted
    /// by score DESC. Returns an empty Vec if no rows exist yet
    /// (pre-P3 rooms get backfilled on the next ensure_db migration).
    pub async fn list_room_ranks(
        &self,
        room_id: &str,
    ) -> Result<Vec<RankSnapshot>, String> {
        self.db.list_room_ranks(room_id)
    }

    /// P3 — adjust a member's score by `delta` (positive = reward,
    /// negative = penalty). Idempotent on missing rows — creates the
    /// row at the default score first, then applies the delta. Used
    /// by future task-completion / violation hooks (not wired yet).
    pub async fn adjust_member_score(
        &self,
        room_id: &str,
        agent_id: &str,
        delta: i32,
    ) -> Result<RankSnapshot, String> {
        self.db.adjust_member_score(room_id, agent_id, delta)
    }

    /// Read an agent's current score in a room. Returns `Ok(None)` if the
    /// agent has no recorded rank yet.
    pub async fn get_agent_score(
        &self,
        room_id: &str,
        agent_id: &str,
    ) -> Result<Option<i32>, String> {
        self.db.get_agent_score(room_id, agent_id)
    }

    /// Force-set an agent's score (used by Yanluo audit). Negative values
    /// are clamped to 0 at the storage layer.
    pub async fn set_agent_score(
        &self,
        room_id: &str,
        agent_id: &str,
        new_score: i32,
    ) -> Result<(), String> {
        self.db.set_agent_score(room_id, agent_id, new_score)
    }

    /// P1.2 — bump or reset the agent's consecutive-pass streak.
    /// Pass: streak += 1, max_streak = max(max, streak). Fail: streak = 0.
    /// Returns the new streak so the caller can decide on milestone bonuses.
    /// Used by `runtime::on_review_verdict` after the ±5 score hook.
    pub async fn bump_streak(
        &self,
        room_id: &str,
        agent_id: &str,
        passed: bool,
    ) -> Result<u32, String> {
        self.db.bump_streak(room_id, agent_id, passed)
    }

    /// Engine-side bridge: save a [`super::task_types::TaskNode`] to the
    /// `subtasks` table by translating it into the persistence-layer
    /// `SubTask` shape. Best-effort — errors are returned, not panicked.
    pub fn save_subtask_from_engine(
        &self,
        room_id: &str,
        node: &super::task_types::TaskNode,
    ) -> Result<(), String> {
        let parent_task_id = node
            .parent_id
            .clone()
            .unwrap_or_else(|| format!("room-{}", room_id));
        let subtask = SubTask {
            id: node.id.clone(),
            task_id: parent_task_id,
            description: node.description.clone(),
            assigned_agent_id: node.assigned_agent.clone(),
            session_id: node.room_id.clone(),
            status: match node.phase {
                super::task_types::TaskPhase::Idle => SubTaskStatus::Pending,
                super::task_types::TaskPhase::Planning => SubTaskStatus::Pending,
                super::task_types::TaskPhase::Executing => SubTaskStatus::Running,
                super::task_types::TaskPhase::Review => SubTaskStatus::Running,
                super::task_types::TaskPhase::Revising => SubTaskStatus::Running,
                super::task_types::TaskPhase::Completed => SubTaskStatus::Done,
                super::task_types::TaskPhase::Cancelled => SubTaskStatus::Failed,
            },
            result: if !node.output.is_empty() {
                Some(node.output.clone())
            } else {
                None
            },
            started_at: Some(node.created_at),
            completed_at: if matches!(node.phase, super::task_types::TaskPhase::Completed) {
                Some(node.updated_at)
            } else {
                None
            },
            review: node.review.clone(),
        };
        self.db.save_subtask(&subtask)
    }

    /// Persist `settings` to the DB, then reflect the new `allow_idle_chat`
    /// state on the in-memory idle timer.
    ///
    /// Behavior matrix:
    /// - `allow_idle_chat=true`  AND `idle_interval_secs > 0` AND idle is
    ///   constructed → spawn (or restart) the room's timer using the
    ///   current member list as candidates.
    /// - otherwise → stop any running timer for this room.
    ///
    /// The member list is **snapshotted at call time** — agents invited
    /// later will not retroactively join the timer until `update_settings`
    /// is called again (e.g. the user toggles something in the UI).
    /// This keeps the timer loop's `&agents` borrow trivially safe.
    pub async fn update_settings(&self, room_id: &str, settings: &RoomSettings) -> Result<(), String> {
        self.db.update_settings(room_id, settings)?;
        self.apply_idle_settings(room_id, settings).await;
        Ok(())
    }

    /// Mirror `settings.allow_idle_chat` onto the runtime timer.
    /// Split out from `update_settings` so other code paths (e.g. a
    /// future "reset to defaults" endpoint, or migration) can apply
    /// the same side effect without re-writing to the DB.
    async fn apply_idle_settings(&self, room_id: &str, settings: &RoomSettings) {
        let Some(idle) = self.idle.lock().clone() else {
            return;
        };
        if settings.allow_idle_chat && settings.idle_interval_secs > 0 {
            match self.db.get_agents(room_id) {
                Ok(agents) => {
                    idle.start_room(room_id, settings.idle_interval_secs, agents).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "apply_idle_settings: failed to load agents for room {}: {}",
                        room_id, e
                    );
                }
            }
        } else {
            idle.stop_room(room_id).await;
        }
    }

    // ── Message Handling ──────────────────────────────────────

    pub async fn send_message(
        &self,
        room_id: &str,
        sender_id: &str,
        sender_name: &str,
        content: &str,
    ) -> Result<ChatMessage, String> {
        let msg = ChatMessage {
            id: Uuid::new_v4().to_string(),
            room_id: room_id.to_string(),
            sender_id: sender_id.to_string(),
            sender_name: sender_name.to_string(),
            content: content.to_string(),
            message_type: MessageType::Chat,
            metadata: None,
            created_at: Utc::now(),
        };
        self.db.save_message(&msg)?;
        Ok(msg)
    }

    pub async fn save_agent_message(
        &self,
        room_id: &str,
        agent_id: &str,
        agent_name: &str,
        content: &str,
        msg_type: MessageType,
    ) -> Result<ChatMessage, String> {
        self.save_agent_message_with_meta(
            room_id,
            agent_id,
            agent_name,
            content,
            msg_type,
            None,
        ).await
    }

    /// Like `save_agent_message` but also stores an optional metadata JSON
    /// blob (e.g. the raw unstripped agent output so the frontend can render
    /// a collapsible "thinking" section).
    pub async fn save_agent_message_with_meta(
        &self,
        room_id: &str,
        agent_id: &str,
        agent_name: &str,
        content: &str,
        msg_type: MessageType,
        metadata: Option<String>,
    ) -> Result<ChatMessage, String> {
        let msg = ChatMessage {
            id: Uuid::new_v4().to_string(),
            room_id: room_id.to_string(),
            sender_id: agent_id.to_string(),
            sender_name: agent_name.to_string(),
            content: content.to_string(),
            message_type: msg_type,
            metadata,
            created_at: Utc::now(),
        };
        self.db.save_message(&msg)?;
        Ok(msg)
    }

    pub async fn get_messages(&self, room_id: &str, limit: u32) -> Result<Vec<ChatMessage>, String> {
        self.db.get_messages(room_id, limit)
    }

    /// M4 — paginated message loading with cursor support.
    pub async fn get_messages_paginated(
        &self,
        room_id: &str,
        before_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ChatMessage>, String> {
        self.db.get_messages_paginated(room_id, before_id, limit)
    }

    /// Build the room context string for agent system prompts.
    pub fn build_room_context(messages: &[ChatMessage], max_messages: usize) -> String {
        let recent: Vec<&ChatMessage> = messages.iter().rev().take(max_messages).rev().collect();
        recent
            .iter()
            .map(|m| format!("{}: {}", m.sender_name, m.content))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── Task Management ──────────────────────────────────────

    pub async fn create_task(
        &self,
        room_id: &str,
        leader_agent_id: &str,
        prompt: &str,
    ) -> Result<GroupTask, String> {
        let task = GroupTask {
            id: Uuid::new_v4().to_string(),
            room_id: room_id.to_string(),
            leader_agent_id: leader_agent_id.to_string(),
            original_prompt: prompt.to_string(),
            plan_json: None,
            status: TaskStatus::Pending,
            final_report: None,
            approved_at: None,
            created_at: Utc::now(),
            completed_at: None,
        };
        self.db.save_task(&task)?;

        // Update runtime
        let mut runtime = self.runtime.write().await;
        let room_runtime = runtime.entry(room_id.to_string()).or_default();
        room_runtime.active_task_id = Some(task.id.clone());

        Ok(task)
    }

    pub async fn update_task_status(&self, task_id: &str, status: TaskStatus) -> Result<(), String> {
        if let Some(mut task) = self.db.get_task(task_id)? {
            task.status = status;
            if task.status == TaskStatus::Completed {
                task.completed_at = Some(Utc::now());
            }
            self.db.save_task(&task)?;
        }
        Ok(())
    }

    pub async fn get_subtasks(&self, task_id: &str) -> Result<Vec<SubTask>, String> {
        self.db.get_subtasks(task_id)
    }

    pub async fn save_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        self.db.save_subtask(subtask)
    }

    /// Get the active task for a room (newest in-flight). Returns
    /// `Ok(None)` if no active task exists.
    pub async fn get_active_task(&self, room_id: &str) -> Result<Option<GroupTask>, String> {
        self.db.get_active_task_for_room(room_id)
    }

    /// List all tasks for a room, newest first. Used for the history
    /// view in the task panel drawer.
    pub async fn list_tasks_for_room(&self, room_id: &str) -> Result<Vec<GroupTask>, String> {
        self.db.list_tasks_for_room(room_id)
    }

    /// Update an existing subtask in place. Returns
    /// `Err("subtask not found")` if no row matches.
    pub async fn update_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        self.db.update_subtask(subtask)
    }

    // ── Announcements ──────────────────────────────────────────

    /// Publish (or replace) the room's pinned announcement. Pass
    /// `Some(text)` to publish, `None` to clear. Captures the
    /// published-at timestamp from the manager's clock so the
    /// persistence layer doesn't need to depend on one of its own.
    pub async fn set_announcement(
        &self,
        room_id: &str,
        text: Option<&str>,
    ) -> Result<(), String> {
        let published_at = if text.is_some() { Some(Utc::now()) } else { None };
        self.db.set_announcement(room_id, text, published_at)
    }

    // ── Leader Access ──────────────────────────────────────────

    pub fn leader_orchestrator(&self) -> &LeaderOrchestrator {
        &self.leader
    }

    /// Look up the display name for `agent_id` in `room_id`.
    /// Falls back to `agent_id` itself when no profile row is found
    /// (e.g. the built-in `__leader__` sentinel has no `display_name`
    /// field on `AgentRoomProfile` — the caller can substitute its own
    /// label like `"项目总管"` in that case).
    pub fn agent_display_name(&self, room_id: &str, agent_id: &str) -> String {
        self.db
            .get_agents(room_id)
            .ok()
            .and_then(|agents| {
                agents
                    .iter()
                    .find(|a| a.agent_id == agent_id)
                    .map(|a| a.agent_id.clone())
            })
            .unwrap_or_else(|| agent_id.to_string())
    }

    /// Build the leader system prompt for a specific room. The prompt
    /// includes the team roster so the leader knows who it can dispatch
    /// subtasks to. Centralized here so the runtime does not need
    /// direct access to `db` or to the `LeaderOrchestrator` API
    /// surface (both of which would otherwise leak as private-field
    /// accesses across sibling modules).
    pub fn build_leader_prompt_for(&self, room_id: &str) -> String {
        let agents = self.db.get_agents(room_id).unwrap_or_default();
        let agent_names: Vec<(String, String)> = agents
            .iter()
            .map(|a| (a.agent_id.clone(), a.agent_id.clone()))
            .collect();
        LeaderOrchestrator::build_leader_prompt(&agents, &agent_names)
    }

    // ── Agent Dispatch ──────────────────────────────────────────

    /// Dispatch a message to an agent and get a real response.
    pub async fn dispatch_message(
        &self,
        room_id: &str,
        agent_id: &str,
        content: &str,
    ) -> Result<ChatMessage, String> {
        let dispatcher = self.dispatcher.lock()
            .as_ref()
            .cloned()
            .ok_or("No agent dispatcher configured")?;

        // Find agent profile in room
        let agents = self.db.get_agents(room_id)?;
        let profile = agents.iter().find(|a| a.agent_id == agent_id)
            .ok_or(format!("Agent {} not in room {}", agent_id, room_id))?;

        // Build room context from recent messages
        let messages = self.db.get_messages(room_id, 20)?;
        let room_context = Self::build_room_context(&messages, 20);

        // Build system prompt
        let system_prompt = format!(
            "你是{}，在群聊室 '{}' 中。\n\n房间上下文：\n{}\n\n请回复用户的消息。",
            profile.agent_id, room_id, room_context
        );

        // Dispatch to agent
        let response = dispatcher.dispatch_to_agent(
            room_id,
            agent_id,
            &profile.session_id,
            &system_prompt,
            content,
            &room_context,
        ).await?;

        // Save agent response
        self.save_agent_message(
            room_id,
            agent_id,
            &profile.agent_id,
            &response,
            MessageType::Chat,
        ).await
    }
}
