use std::sync::Arc;
use async_trait::async_trait;

use hermix_config::paths;
use hermix_core::traits::{AgentService, AgentOverrides};
use hermix_core::types::StreamChunk;
use hermix_gateway::groupchat::dispatcher::{AgentDispatcher, StreamDelta};
use hermix_gateway::groupchat::manager::GroupChatManager;

/// Server-side implementation of AgentDispatcher using the existing AgentService.
///
/// Holds a reference to the `GroupChatManager` so it can look up per-agent
/// profiles (model override, display name) when dispatching. This was
/// previously broken — `_agent_id` was unused, meaning every agent in a
/// room got the same default model regardless of their `model_override`
/// setting.
pub struct ServerAgentDispatcher {
    agent_service: Arc<dyn AgentService>,
    manager: Arc<GroupChatManager>,
}

impl ServerAgentDispatcher {
    pub fn new(agent_service: Arc<dyn AgentService>, manager: Arc<GroupChatManager>) -> Self {
        Self { agent_service, manager }
    }

    /// Derive the per-room memory directory used by every agent in a group
    /// chat. All members (manager + participants) of the same `room_id`
    /// share this directory, while different rooms — and private 1:1 chats —
    /// never observe each other's data. This is the directory isolation that
    /// stops a group chat from reading memories stored during a private
    /// conversation.
    fn room_memory_dir(room_id: &str) -> String {
        paths::hermix_home()
            .join("data")
            .join("groupchat")
            .join(room_id)
            .join("memories")
            .to_string_lossy()
            .to_string()
    }

    /// Look up the agent's per-room model override. Returns `None` when
    /// the agent has no profile in this room or no override is set.
    fn agent_model_override(&self, room_id: &str, agent_id: &str) -> Option<String> {
        self.manager
            .get_room_agents_sync(room_id)
            .ok()
            .and_then(|agents| {
                agents
                    .iter()
                    .find(|a| a.agent_id == agent_id)
                    .and_then(|a| a.model_override.clone())
            })
    }

    /// Build the `AgentOverrides` for a dispatch, incorporating the
    /// per-room model override if one is set for this agent.
    fn build_overrides(&self, room_id: &str, agent_id: &str, system_prompt: &str) -> AgentOverrides {
        AgentOverrides {
            system_prompt: Some(system_prompt.to_string()),
            memory_dir: Some(Self::room_memory_dir(room_id)),
            model: self.agent_model_override(room_id, agent_id),
            ..Default::default()
        }
    }
}

#[async_trait]
impl AgentDispatcher for ServerAgentDispatcher {
    async fn dispatch_to_agent(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        _room_context: &str,
    ) -> Result<String, String> {
        let overrides = self.build_overrides(room_id, agent_id, system_prompt);

        let reply = self.agent_service
            .send_message(session_id, message, overrides)
            .await
            .map_err(|e| format!("Agent dispatch failed: {}", e))?;

        Ok(reply.text)
    }

    async fn dispatch_subtask(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        task_description: &str,
        system_prompt: &str,
    ) -> Result<String, String> {
        let overrides = self.build_overrides(room_id, agent_id, system_prompt);

        let reply = self.agent_service
            .send_message(session_id, task_description, overrides)
            .await
            .map_err(|e| format!("Agent dispatch failed: {}", e))?;

        Ok(reply.text)
    }

    async fn dispatch_subtask_stream(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        _role: &str,
        on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync>,
    ) -> Result<String, String> {
        let overrides = self.build_overrides(room_id, agent_id, system_prompt);

        // Bridge: chunk from AgentService → tiny delta to the gateway
        // callback. We only forward `delta.content` so the runtime never
        // has to learn about StreamChunk internals.
        let bridge: Arc<dyn Fn(StreamChunk) + Send + Sync> = Arc::new(move |chunk: StreamChunk| {
            if let Some(d) = chunk.delta {
                if let Some(content) = d.content {
                    if !content.is_empty() {
                        on_chunk(StreamDelta { content });
                    }
                }
            }
        });

        let reply = self.agent_service
            .send_message_stream(session_id, message, overrides, bridge)
            .await
            .map_err(|e| format!("Agent streaming dispatch failed: {}", e))?;

        Ok(reply.text)
    }
}
