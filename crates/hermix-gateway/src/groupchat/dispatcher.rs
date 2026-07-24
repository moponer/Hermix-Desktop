use async_trait::async_trait;

/// Abstraction for dispatching messages to agents.
///
/// Defined in hermix-gateway, implemented in hermix-server
/// (which has access to both hermix-agent and hermix-gateway).
#[async_trait]
pub trait AgentDispatcher: Send + Sync {
    /// Send a message to a specific agent in a room and get the response.
    async fn dispatch_to_agent(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        room_context: &str,
    ) -> Result<String, String>;

    /// Dispatch a task to a sub-agent (for leader delegation).
    async fn dispatch_subtask(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        task_description: &str,
        system_prompt: &str,
    ) -> Result<String, String>;

    /// Stream a dispatch: each LLM chunk is forwarded to `on_chunk` as it
    /// arrives, so the runtime can publish a `RoomEvent::AgentThinkingChunk`
    /// per chunk and render a typing/streaming indicator on the frontend.
    /// The final `Result<String, String>` is the assembled reply text.
    /// /// `role` is a free-form tag (`"leader"` / `"member"`) the frontend
    /// can use to pick an avatar tint or badge; the dispatcher itself
    /// does not interpret it.
    async fn dispatch_subtask_stream(
        &self,
        room_id: &str,
        agent_id: &str,
        session_id: &str,
        system_prompt: &str,
        message: &str,
        role: &str,
        on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync>,
    ) -> Result<String, String>;
}

/// Minimal chunk shape — matches `hermix_core::types::StreamDelta` but
/// kept local so the dispatcher trait does not leak a core-types
/// dependency into every implementor.
#[derive(Debug, Clone)]
pub struct StreamDelta {
    pub content: String,
}