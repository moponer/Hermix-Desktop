//! Trait for tools that need to know the **current session** when
//! broadcasting events.
//!
//! Why this trait exists: `ToolHandler::execute` only receives
//! `params: Value`, and the LLM provider doesn't put `session_id`
//! in the JSON params. Tools that fan out side events (the `todo`
//! tool's task-bus, for example) need a way to stamp the originating
//! session onto each event so the WS layer can filter per session.
//!
//! `agent_loop.execute_tool_calls` calls
//! `ToolRegistry::set_active_session_for_tool("todo", session_id)`
//! right before the JoinSet batch, then resets it on scope exit.
//! The lookup uses `Arc::downcast` against the registered
//! `Arc<dyn ToolHandler>`, so non-conforming tools (the vast
//! majority) pay zero cost.

/// Marker trait: a tool that wants to know the currently-executing
/// session id before each tool-call batch.
///
/// The session id is **transient** — set before the batch, reset
/// after. Tools must never store it; they read it inside `publish()`
/// or analogous fan-out paths.
pub trait SessionStamper: Send + Sync {
    /// Stamp or clear the active session. Returns the previous
    /// session so callers can restore it on scope exit.
    fn set_active_session(&self, session_id: Option<String>) -> Option<String>;
}
