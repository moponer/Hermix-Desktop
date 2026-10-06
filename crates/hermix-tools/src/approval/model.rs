//! Approval data model (decisions, terminal context).
use hermix_core::ApprovalOutcome;
/// Decision from the approval check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Command is safe to execute without confirmation.
    Approved,
    /// Command is denied outright.
    Denied,
    /// Command requires user confirmation before execution.
    RequiresConfirmation,
}
// ---------------------------------------------------------------------------
//TerminalApprovalContext (, "Caution tool needs UI prompts")
// ---------------------------------------------------------------------------

/// Per-request hook the `terminal` tool uses to surface a
/// `RequiresConfirmation` decision as a `request_approval` event on the
/// same WS channel the agent streams on, instead of bouncing the
/// the LLM-faked-confirmation text flow. Removed 2026-10-06: the model could
/// Set by `LocalAgentService::send_message` on every turn (the closure
/// captures the active WS `ApprovalRegistry` + `on_event`), and read by
/// `TerminalHandler::execute` when `ApprovalManager::check_approval`
/// returns `RequiresConfirmation`. When both fields are `None`
/// (unit tests, headless callers, CLI runs without a UI), the tool
/// falls back to the legacy text-based flow — backward-compatible.
/// The closure captures the same `Arc`s the dangerous-tool approval
/// path uses (see `local_agent_service::decide_pre_tool_dispatch`),
/// so the frontend can reuse the existing `InlineApprovalCard` without
/// any client-side change.
#[derive(Default, Clone)]
pub struct TerminalApprovalContext {
    /// Session id forwarded to the `requester` closure. The terminal
    /// tool itself doesn't know its session id (the dispatcher is the
    /// agent service), so the caller writes it here when wiring the
    /// per-request hook. Empty string = "no WS session".
    pub session_id: String,
    /// `(session_id, request_id) -> oneshot::Receiver<ApprovalOutcome>`.
    /// The tool builds the `request_id`, calls this to register the
    /// pending decision, then `.await`s the receiver for the user's
    /// verdict. `ApprovalOutcome` distinguishes `Approved` from
    /// `DeniedByUser` / `TimedOut` / `ChannelClosed` so the tool can
    /// emit an accurate reply (A+C product fix, ). None →
    /// fallback to text flow.
    pub requester: Option<
        std::sync::Arc<
            dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome>
                + Send
                + Sync,
        >,
    >,
    /// Emits a JSON envelope on the active WS stream. Used to push
    /// `{"event":"request_approval",...}` so the desktop UI renders
    /// the approval card. None → fallback to text flow.
    pub on_event: Option<std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>>,
}

impl TerminalApprovalContext {
    /// True when all three fields are populated — only then will the
    /// tool emit the WS event; otherwise the legacy "tell the user to
    /// write that prefix itself, so it granted itself approval.
    pub fn is_wired(&self) -> bool {
        !self.session_id.is_empty() && self.requester.is_some() && self.on_event.is_some()
    }

    /// Helper for the tool side: clone the requester Arc (or `None`).
    pub fn requester(
        &self,
    ) -> Option<
        std::sync::Arc<
            dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome>
                + Send
                + Sync,
        >,
    > {
        self.requester.clone()
    }

    /// Helper for the tool side: clone the on_event Arc (or `None`).
    pub fn on_event(&self) -> Option<std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>> {
        self.on_event.clone()
    }
}
