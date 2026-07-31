//! Per-session tool execution context.
//!
//! ## Why this module exists
//!
//! Before (session isolation), per-session state (workspace,
//! approval context, permission profile, session id, stdout callback)
//! lived on process-global `Arc<Mutex<…>>` fields on
//! [`crate::registry::ToolRegistryInner`]. `LocalAgentService` wrote
//! to those globals at the start of each turn; tool handlers read
//! from them at dispatch time. When two sessions ran concurrently,
//! session B's writes clobbered session A's values before A's tool
//! dispatch completed — a cross-session race.
//!
//! `ToolCallContext` fixes this by **carrying** per-session state
//! through the dispatch chain instead of storing it globally. The
//! bridge closure in `agent_builder::bridge_tool_registry` captures
//! an `Arc<ToolCallContext>` and forwards it to
//! `ToolRegistry::dispatch_with_context`, which passes it to the
//! `ContextAware::execute_with_context` method of tools that opted
//! in. Tools that don't need session context keep using
//! `ToolHandler::execute` unchanged.
//!
//! ## Pattern
//!
//! This follows the same opt-in pattern as [`crate::SessionStamper`]:
//! `ToolEntry` has an optional `context_aware:
//! Option<Arc<dyn ContextAware>>`. Only the 3 tools that need
//! per-session state (terminal, todo, code_execution) implement
//! `ContextAware`; the other 27+ pay zero cost.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermix_core::{PermissionProfile, ToolError};
use serde_json::Value;

use crate::approval::TerminalApprovalContext;

// ---------------------------------------------------------------------------
// ToolCallContext
// ---------------------------------------------------------------------------

/// Per-session execution context, threaded through the tool dispatch
/// chain. Eliminates the need for process-global mutable state on
/// `ToolRegistryInner`.
///
/// All fields are `Clone` (closures are behind `Arc`), so the entire
/// struct is `Clone` — it can be cloned into each `JoinSet::spawn`
/// task in `execute_tool_calls` safely.
///
/// Constructed by `LocalAgentService` at the start of each
/// `send_message` / `send_message_stream` call from the session's
/// overrides and config. Passed to `bridge_tool_registry` which
/// captures it in the bridge closures.
#[derive(Clone, Default)]
pub struct ToolCallContext {
    /// The session this tool call belongs to. Used by `todo` to
    /// stamp events and by `terminal` to route approval requests.
    pub session_id: String,

    /// Per-turn workspace root. `terminal` uses this as the default
    /// `current_dir` when the LLM doesn't pass an explicit `workdir`.
    pub workspace: Option<PathBuf>,

    /// Per-request approval hook for the terminal tool. When
    /// populated, `RequiresConfirmation` is surfaced as a
    /// `request_approval` event on the active WS channel.
    pub approval_ctx: TerminalApprovalContext,

    /// Per-session permission profile. `terminal` reads this to
    /// skip its internal `ApprovalManager` for pre-granted commands.
    pub permission_profile: PermissionProfile,

    /// Wire-format assistant message id (`${session_id}-${rowIdx}`).
    /// Used by `todo` to anchor inline cards to the correct message.
    pub message_id: Option<String>,

    /// Per-session stdout streaming callback for `code_execution`.
    /// Wrapped in `Arc<Mutex>` so `LocalAgentService` can set/clear it
    /// mid-stream (the context is captured at bridge time; the shared
    /// Arc handle allows dynamic updates without re-bridging).
    pub stdout_cb: Arc<Mutex<Option<Box<dyn Fn(&str) + Send + Sync>>>>,

    /// Active `tool_call_id` for the currently running `code_execution`.
    /// Updated per tool-call by the stream loop so the stdout callback
    /// can emit `tool_stdout` events with the correct ID.
    pub active_call_id: Arc<Mutex<Option<String>>>,
}

impl ToolCallContext {
    /// Create a context with just a session id (all other fields
    /// default). Useful for tests and headless callers that don't
    /// need workspace/approval wiring.
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// ContextAware trait
// ---------------------------------------------------------------------------

/// Opt-in trait for tools that need per-session context at dispatch
/// time. Follows the same pattern as [`crate::SessionStamper`]:
/// `ToolEntry` stores an `Option<Arc<dyn ContextAware>>` alongside
/// the `Arc<dyn ToolHandler>`. Only tools that need session-scoped
/// state (terminal, todo, code_execution) implement this trait.
///
/// When `ToolRegistry::dispatch_with_context` is called:
/// - If the tool has a `ContextAware` impl, `execute_with_context`
///   is called with the `ToolCallContext`.
/// - Otherwise, the standard `ToolHandler::execute` is called
///   (context is ignored — backward compatible).
///
/// Both paths converge on the same truncation / error-wrapping logic
/// in `dispatch_with_context`.
#[async_trait]
pub trait ContextAware: Send + Sync {
    /// Execute the tool with the given parameters and per-session
    /// context. The context provides workspace, approval hooks,
    /// session id, and other per-session state that was previously
    /// read from process-global `Arc<Mutex<…>>` fields.
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError>;
}
