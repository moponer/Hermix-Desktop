use std::sync::Arc;
use async_trait::async_trait;

use hermix_core::traits::{AgentService, AgentOverrides};
use hermix_core::types::StreamChunk;
use hermix_gateway::groupchat::dispatcher::{AgentDispatcher, StreamDelta};

/// Server-side implementation of AgentDispatcher using the existing AgentService.
pub struct ServerAgentDispatcher {
    agent_service: Arc<dyn AgentService>,
}

impl ServerAgentDispatcher {
    pub fn new(agent_service: Arc<dyn AgentService>) -> Self {
        Self { agent_service }
    }
}

#[async_trait]
impl AgentDispatcher for ServerAgentDispatcher {
    async fn dispatch_to_agent(
        &self,
        _room_id: &str,
        _agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        _room_context: &str,
    ) -> Result<String, String> {
        let overrides = AgentOverrides {
            system_prompt: Some(system_prompt.to_string()),
            ..Default::default()
        };

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
        _system_prompt: &str,
    ) -> Result<String, String> {
        self.dispatch_to_agent(
            room_id,
            agent_id,
            session_id,
            "",
            task_description,
            "",
        ).await
    }

    async fn dispatch_subtask_stream(
        &self,
        _room_id: &str,
        _agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        _role: &str,
        on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync>,
    ) -> Result<String, String> {
        let overrides = AgentOverrides {
            system_prompt: Some(system_prompt.to_string()),
            ..Default::default()
        };

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