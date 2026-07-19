use std::collections::HashMap;
use std::sync::Mutex;
use chrono::Utc;
use uuid::Uuid;

use super::persistence::*;
// `Uuid` import is still used below for room id (RoomPersistence returns
// its own UUID) — keep it explicit so future readers don't strip it.

/// In-memory implementation of RoomPersistence for development/testing.
pub struct InMemoryRoomPersistence {
    rooms: Mutex<HashMap<String, GroupChatRoom>>,
    agents: Mutex<Vec<AgentRoomProfile>>,
    messages: Mutex<Vec<ChatMessage>>,
    tasks: Mutex<Vec<GroupTask>>,
    subtasks: Mutex<Vec<SubTask>>,
    /// P3 勋章 — per-(room, agent) rank snapshot. Lazy insert happens
    /// the first time the row is read or written.
    ranks: Mutex<Vec<RankSnapshot>>,
}

impl InMemoryRoomPersistence {
    pub fn new() -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            agents: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            subtasks: Mutex::new(Vec::new()),
            ranks: Mutex::new(Vec::new()),
        }
    }
}

impl RoomPersistence for InMemoryRoomPersistence {
    fn create_room(&self, name: &str) -> Result<GroupChatRoom, String> {
        let room = GroupChatRoom {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            leader_id: None,
            agent_ids: vec![],
            settings: RoomSettings::default(),
            rules: RoomRules::default(),
            announcement: None,
            announcement_published_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let leader_session_id = new_session_id();
        // Auto-create the built-in leader (总管) profile so the
        // existing leader-driven send_message flow can find it.
        // The leader row is added to `agents` but its `__leader__`
        // sentinel id is NOT pushed into `room.agent_ids` (which is
        // the user-facing member roster).
        let leader_profile = AgentRoomProfile {
            agent_id: LEADER_AGENT_ID.to_string(),
            room_id: room.id.clone(),
            role: AgentRole::Leader,
            session_id: leader_session_id,
            model_override: None,
            display_name: Some("项目总管".to_string()),
        };
        self.agents.lock().unwrap().push(leader_profile);
        self.rooms.lock().unwrap().insert(room.id.clone(), room.clone());
        // P3 — seed the leader's rank row at the default 10/青铜 so
        // the room's first GET /ranks returns a snapshot for every
        // member (including the synthetic leader).
        self.ranks.lock().unwrap().push(
            RankSnapshot::default_for(&room.id, LEADER_AGENT_ID),
        );
        Ok(room)
    }

    fn get_room(&self, id: &str) -> Result<Option<GroupChatRoom>, String> {
        Ok(self.rooms.lock().unwrap().get(id).cloned())
    }

    fn list_rooms(&self) -> Result<Vec<GroupChatRoom>, String> {
        let mut rooms: Vec<GroupChatRoom> = self.rooms.lock().unwrap().values().cloned().collect();
        rooms.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(rooms)
    }

    fn update_room(&self, room: &GroupChatRoom) -> Result<(), String> {
        self.rooms.lock().unwrap().insert(room.id.clone(), room.clone());
        Ok(())
    }

    fn delete_room(&self, id: &str) -> Result<(), String> {
        self.rooms.lock().unwrap().remove(id);
        self.agents.lock().unwrap().retain(|a| a.room_id != id);
        self.messages.lock().unwrap().retain(|m| m.room_id != id);
        self.tasks.lock().unwrap().retain(|t| t.room_id != id);
        self.ranks.lock().unwrap().retain(|r| r.room_id != id);
        Ok(())
    }

    fn add_agent(&self, room_id: &str, agent_id: &str, role: AgentRole) -> Result<AgentRoomProfile, String> {
        let profile = AgentRoomProfile {
            agent_id: agent_id.to_string(),
            room_id: room_id.to_string(),
            role,
            session_id: new_session_id(),
            model_override: None,
            display_name: None,
        };
        self.agents.lock().unwrap().push(profile.clone());
        // Update room agent_ids
        if let Some(room) = self.rooms.lock().unwrap().get_mut(room_id) {
            if !room.agent_ids.contains(&agent_id.to_string()) {
                room.agent_ids.push(agent_id.to_string());
                room.updated_at = Utc::now();
            }
        }
        // P3 — seed a default rank row for this member if it doesn't
        // already have one. Idempotent — re-inviting the same agent
        // does NOT reset their existing score.
        let mut ranks = self.ranks.lock().unwrap();
        let already = ranks.iter().any(|r| r.room_id == room_id && r.agent_id == agent_id);
        if !already {
            ranks.push(RankSnapshot::default_for(room_id, agent_id));
        }
        Ok(profile)
    }

    fn remove_agent(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        self.agents.lock().unwrap().retain(|a| !(a.room_id == room_id && a.agent_id == agent_id));
        // P3 — drop the member's rank row too so /ranks stays in sync.
        self.ranks.lock().unwrap().retain(|r| !(r.room_id == room_id && r.agent_id == agent_id));
        if let Some(room) = self.rooms.lock().unwrap().get_mut(room_id) {
            room.agent_ids.retain(|a| a != agent_id);
            room.updated_at = Utc::now();
        }
        Ok(())
    }

    fn get_agents(&self, room_id: &str) -> Result<Vec<AgentRoomProfile>, String> {
        Ok(self.agents.lock().unwrap().iter().filter(|a| a.room_id == room_id).cloned().collect())
    }

    fn set_leader(&self, room_id: &str, agent_id: &str) -> Result<(), String> {
        if let Some(room) = self.rooms.lock().unwrap().get_mut(room_id) {
            room.leader_id = Some(agent_id.to_string());
            room.updated_at = Utc::now();
        }
        Ok(())
    }

    fn set_member_model(
        &self,
        room_id: &str,
        agent_id: &str,
        model: Option<&str>,
    ) -> Result<(), String> {
        let mut agents = self.agents.lock().unwrap();
        for a in agents.iter_mut() {
            if a.room_id == room_id && a.agent_id == agent_id {
                a.model_override = model.map(|s| s.to_string());
                return Ok(());
            }
        }
        Err(format!("agent {} not in room {}", agent_id, room_id))
    }

    fn set_member_display_name(
        &self,
        room_id: &str,
        agent_id: &str,
        display_name: Option<&str>,
    ) -> Result<(), String> {
        // Treat empty / whitespace-only as "clear alias" so the API
        // layer can stay naive (clients send "" → we store None).
        let normalized = display_name.map(str::trim).filter(|s| !s.is_empty());
        let mut agents = self.agents.lock().unwrap();
        for a in agents.iter_mut() {
            if a.room_id == room_id && a.agent_id == agent_id {
                a.display_name = normalized.map(str::to_string);
                return Ok(());
            }
        }
        Err(format!("agent {} not in room {}", agent_id, room_id))
    }

    fn list_room_ranks(&self, room_id: &str) -> Result<Vec<RankSnapshot>, String> {
        let ranks = self.ranks.lock().unwrap();
        Ok(ranks.iter().filter(|r| r.room_id == room_id).cloned().collect())
    }

    fn adjust_member_score(
        &self,
        room_id: &str,
        agent_id: &str,
        delta: i32,
    ) -> Result<RankSnapshot, String> {
        let mut ranks = self.ranks.lock().unwrap();
        // Upsert — if the row doesn't exist yet, create it at the
        // default score first, then apply the delta.
        let pos = ranks.iter().position(|r| r.room_id == room_id && r.agent_id == agent_id);
        let row = match pos {
            Some(i) => &mut ranks[i],
            None => {
                ranks.push(RankSnapshot::default_for(room_id, agent_id));
                let last = ranks.len() - 1;
                &mut ranks[last]
            }
        };
        row.score = row.score.saturating_add(delta);
        if delta > 0 {
            row.total_tasks = row.total_tasks.saturating_add(1);
            row.consecutive_no_violation = row.consecutive_no_violation.saturating_add(1);
        } else if delta < 0 {
            row.consecutive_no_violation = 0;
        }
        row.refresh_tier();
        Ok(row.clone())
    }

    fn get_agent_score(&self, room_id: &str, agent_id: &str) -> Result<Option<i32>, String> {
        let ranks = self.ranks.lock().unwrap();
        Ok(ranks
            .iter()
            .find(|r| r.room_id == room_id && r.agent_id == agent_id)
            .map(|r| r.score))
    }

    fn set_agent_score(&self, room_id: &str, agent_id: &str, new_score: i32) -> Result<(), String> {
        let clamped = new_score.max(-5);
        let mut ranks = self.ranks.lock().unwrap();
        let pos = ranks.iter().position(|r| r.room_id == room_id && r.agent_id == agent_id);
        let row = match pos {
            Some(i) => &mut ranks[i],
            None => {
                ranks.push(RankSnapshot::default_for(room_id, agent_id));
                let last = ranks.len() - 1;
                &mut ranks[last]
            }
        };
        row.score = clamped;
        row.refresh_tier();
        Ok(())
    }

    fn bump_streak(
        &self,
        room_id: &str,
        agent_id: &str,
        passed: bool,
    ) -> Result<u32, String> {
        let mut ranks = self.ranks.lock().unwrap();
        // Upsert — same idempotency contract as `adjust_member_score`.
        let pos = ranks.iter().position(|r| r.room_id == room_id && r.agent_id == agent_id);
        let row = match pos {
            Some(i) => &mut ranks[i],
            None => {
                ranks.push(RankSnapshot::default_for(room_id, agent_id));
                let last = ranks.len() - 1;
                &mut ranks[last]
            }
        };
        if passed {
            // saturating_add: streak of u32::MAX would otherwise wrap
            // and silently *reduce* the streak we just awarded.
            row.consecutive_passes = row.consecutive_passes.saturating_add(1);
            if row.consecutive_passes > row.max_streak {
                row.max_streak = row.consecutive_passes;
            }
        } else {
            row.consecutive_passes = 0;
        }
        Ok(row.consecutive_passes)
    }

    fn update_settings(&self, room_id: &str, settings: &RoomSettings) -> Result<(), String> {
        if let Some(room) = self.rooms.lock().unwrap().get_mut(room_id) {
            room.settings = settings.clone();
            room.updated_at = Utc::now();
        }
        Ok(())
    }

    fn save_message(&self, msg: &ChatMessage) -> Result<(), String> {
        self.messages.lock().unwrap().push(msg.clone());
        Ok(())
    }

    fn get_messages(&self, room_id: &str, limit: u32) -> Result<Vec<ChatMessage>, String> {
        let msgs: Vec<ChatMessage> = self.messages.lock().unwrap()
            .iter()
            .filter(|m| m.room_id == room_id)
            .cloned()
            .collect();
        let len = msgs.len();
        Ok(msgs.into_iter().skip(len.saturating_sub(limit as usize)).collect())
    }

    fn save_task(&self, task: &GroupTask) -> Result<(), String> {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|t| t.id != task.id);
        tasks.push(task.clone());
        Ok(())
    }

    fn get_task(&self, id: &str) -> Result<Option<GroupTask>, String> {
        Ok(self.tasks.lock().unwrap().iter().find(|t| t.id == id).cloned())
    }

    fn save_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        let mut subs = self.subtasks.lock().unwrap();
        subs.retain(|s| s.id != subtask.id);
        subs.push(subtask.clone());
        Ok(())
    }

    fn get_subtasks(&self, task_id: &str) -> Result<Vec<SubTask>, String> {
        Ok(self.subtasks.lock().unwrap().iter().filter(|s| s.task_id == task_id).cloned().collect())
    }

    fn get_active_task_for_room(&self, room_id: &str) -> Result<Option<GroupTask>, String> {
        let active_statuses = [
            TaskStatus::Pending,
            TaskStatus::Decomposing,
            TaskStatus::Executing,
            TaskStatus::Aggregating,
            TaskStatus::AwaitingApproval,
        ];
        let tasks = self.tasks.lock().unwrap();
        let mut best: Option<&GroupTask> = None;
        for t in tasks.iter() {
            if t.room_id == room_id && active_statuses.contains(&t.status) {
                if best.map_or(true, |b| t.created_at > b.created_at) {
                    best = Some(t);
                }
            }
        }
        Ok(best.cloned())
    }

    fn list_tasks_for_room(&self, room_id: &str) -> Result<Vec<GroupTask>, String> {
        let mut tasks: Vec<GroupTask> = self.tasks.lock().unwrap()
            .iter()
            .filter(|t| t.room_id == room_id)
            .cloned()
            .collect();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(tasks)
    }

    fn update_subtask(&self, subtask: &SubTask) -> Result<(), String> {
        let mut subs = self.subtasks.lock().unwrap();
        if let Some(s) = subs.iter_mut().find(|s| s.id == subtask.id) {
            *s = subtask.clone();
            Ok(())
        } else {
            Err(format!("subtask {} not found", subtask.id))
        }
    }

    fn set_announcement(
        &self,
        room_id: &str,
        text: Option<&str>,
        published_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), String> {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(room_id) {
            room.announcement = text.map(|s| s.to_string());
            room.announcement_published_at = published_at;
            room.updated_at = Utc::now();
            Ok(())
        } else {
            Err(format!("room {} not found", room_id))
        }
    }
}
