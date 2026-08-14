//! Registry entry data types.
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use hermix_core::{traits::{RiskLevel, PermissionMode}, ToolHandler, ToolSchema};
use crate::SessionStamper;
use crate::context::{ContextAware};
pub struct ToolEntry {
    /// Unique tool name (e.g. "web_search", "terminal").
    pub name: String,
    /// Which toolset this tool belongs to (e.g. "web", "terminal").
    pub toolset: String,
    /// OpenAI-format tool schema.
    pub schema: ToolSchema,
    /// Handler that executes the tool logic.
    pub handler: Arc<dyn ToolHandler>,
    /// Optional session stamper for tools that need to broadcast
    /// per-session events (currently only `todo`). When `None`,
    /// the tool is session-agnostic. See
    /// `tools::session_stamper::SessionStamper`.
    pub session_stamper: Option<Arc<dyn SessionStamper>>,
    /// Optional context-aware handler for tools that need per-session
    /// state at dispatch time (terminal, todo, code_execution).
    /// When `None`, the tool is session-agnostic and dispatches via
    /// the standard `ToolHandler::execute` path. See
    /// `context::ContextAware`.
    pub context_aware: Option<Arc<dyn ContextAware>>,
    /// Availability check — returns true if the tool can be used right now.
    pub check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Environment dependencies required for this tool (e.g. "EXA_API_KEY").
    pub env_deps: Vec<String>,
    /// Whether this tool performs async I/O.
    pub is_async: bool,
    /// Human-readable description.
    pub description: String,
    /// Emoji icon for display.
    pub emoji: String,
    /// Per-tool max result size override (characters).
    pub max_result_size_chars: Option<usize>,
    /// Risk classification used by the pre-execution approval hook.
    /// Defaults to [`RiskLevel::Caution`] for backwards compatibility —
    /// unknown / newly-added tools get a confirmation prompt rather than
    /// being silently allowed.
    pub risk_level: RiskLevel,
    /// Permission level for this tool (inspired by claw-code design).
    /// Determines what operations the tool is allowed to perform.
    pub permission_mode: PermissionMode,
    /// Whether this tool requires sequential execution (interactive tools
    /// like `clarify`, `delegate_task` must not run in parallel with others).
    /// Set via `ToolRegistry::register_with_risk` and consumed by
    /// `dispatch_tools` to partition calls into parallel vs sequential groups.
    pub is_interactive: bool,
    /// Runtime enable/disable flag, mutated by
    /// [`ToolRegistry::set_toolset_enabled`]. Distinct from
    /// [`Self::check_fn`]: `check_fn` reflects *capability* (env vars
    /// present, browser driver up, etc.), `is_enabled` reflects
    /// *user choice* (the operator toggled this toolset off in
    /// Settings). Both must be true for a tool to be visible to the
    /// LLM and dispatchable. Lock-free: a single `AtomicBool` per
    /// entry, mutated by the Settings UI on a separate task and
    /// read on every dispatch hot path.
    pub is_enabled: Arc<AtomicBool>,
}
#[derive(Debug, Clone)]
pub struct ToolEntryInfo {
    pub name: String,
    pub toolset: String,
    pub description: String,
    pub emoji: String,
    pub is_async: bool,
    pub is_enabled: bool,
    pub env_deps: Vec<String>,
    pub max_result_size_chars: Option<usize>,
}
