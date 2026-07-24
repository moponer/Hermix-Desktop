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

use async_trait::async_trait;

/// Marker trait: a tool that wants to know the currently-executing
/// session id before each tool-call batch.
/// /// The session id is **transient** — set before the batch, reset
/// after. Tools must never store it; they read it inside `publish()`
/// or analogous fan-out paths.
/// /// Now `#[async_trait]` so tools can also expose a session-scoped
/// snapshot builder (Commit B). The only currently-overridden
/// async method is `build_snapshot_json`; all the sync stampers are
/// unaffected.
#[async_trait]
pub trait SessionStamper: Send + Sync {
    /// Stamp or clear the active session. Returns the previous
    /// session so callers can restore it on scope exit.
    fn set_active_session(&self, session_id: Option<String>) -> Option<String>;
    /// Stamp or clear the active **assistant message id** for the
    /// current tool-call batch. Returns the previous id so callers
    /// can restore it on scope exit.
    /// /// Why this exists (P0-T1 Frontend Audit 2026-07-13): the
    /// `todo` tool emits a `TodoGroupStarted` event the first time
    /// it sees a fresh `group_id`. The front-end renders that
    /// group's inline card anchored to the assistant message that
    /// emitted it. Without an explicit id the event arrives with
    /// `message_id: None` and the card falls back to "latest
    /// assistant message older than first item.created_at" which
    /// works in practice but is fragile under tool-call-after-
    /// tool-call patterns.
    /// /// The id is the **wire-format** id (`${session_id}-${rowIdx}`)
    /// that `hermix_server::dashboard::...::get_session_messages`
    /// already uses on read. Computing it once at the top of the
    /// turn (`rowIdx = messages.len()` since the user message was
    /// just pushed) gives a stable anchor that survives reload —
    /// on reload the same `${session_id}-${rowIdx}` formula
    /// reproduces the same id from the same persisted row order.
    fn set_active_message_id(&self, message_id: Option<String>) -> Option<String>;
    /// Build a per-session snapshot of this tool's internal state
    /// as a JSON array. Default impl returns `None` so non-snapshot
    /// tools pay zero cost. The `todo` tool overrides this so the
    /// WS layer can hydrate the front-end todo panel on connect
    /// and after every mutation.
    /// /// Returns `None` for tools that don't have session-scoped
    /// state, `Some(items)` for tools that do. The shape of each
    /// item is tool-specific (the `todo` tool returns TodoItem-
    /// shaped JSON); the caller (`hermix-server`) is responsible
    /// for converting into the wire-format `TodoSnapshotItem`.
    /// /// / Commit B.
    async fn build_snapshot_json(&self, _session_id: &str) -> Option<Vec<serde_json::Value>> {
        None
    }
}