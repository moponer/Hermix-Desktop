//! Approval outcome taxonomy (— A+C product fix).
//!
//! Replaces the previous flat `oneshot::Receiver<bool>` pair with a
//! structured outcome that distinguishes **why** a tool-call approval
//! did or did not grant execution.  The LLM-facing
//! [`ToolDispatchDecision::AwaitApproval`] arm in
//! `hermix-agent::tool_dispatch::resolve_pre_dispatch` consults this
//! enum to (a) build an accurate `tool_result` reason string and
//! (b) trigger the post-tool-iteration **denial system note**
//! (see `tool_result_processor::push_tool_result`).
//!
//! Variants:
//! - [`ApprovalOutcome::Approved`]        — user clicked 允许.
//! - [`ApprovalOutcome::DeniedByUser`]    — user clicked 拒绝.
//! - [`ApprovalOutcome::TimedOut`]        — N-second default-deny timer
//!   in `ApprovalRegistry::request_approval` fired before the user
//!   could decide. Carries the configured timeout length so the agent
//!   can attribute the failure to elapsed time, not user intent.
//! - [`ApprovalOutcome::ChannelClosed`]   — sender dropped before any
//!   value arrived (server crash, WS dropped, agent shutdown, etc.).
//!   This is what replaces the older blank `Err("Approval channel
//!   closed before response.")` string — a string that was conflated
//!   with user denial.
//!
//! `Default::default()` resolves to `DeniedByUser` for ergonomic
//! `match rx.await` defaults; explicit construction is preferred at
//! the call sites so intent is grep-able.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalOutcome {
    /// User explicitly approved the tool call.
    Approved,
    /// User explicitly denied the tool call.
    DeniedByUser,
    /// User denied **and told the agent what to do instead**（审批面板上的
    /// 「告诉模型接下来应该怎么做…」）。note 原样进 LLM 可见的拒绝理由，
    /// 让模型换方案而不是原地重试同一个被拒的工具。
    DeniedWithFeedback { note: String },
    /// The approval registry's default-deny timer fired first.
    /// `timeout_secs` is the configured timeout length (seconds), so
    /// the LLM-facing denial note can phrase the failure correctly
    /// ("你超过了 X 秒没决定，因此工具没有执行" vs "你拒绝了，工具没有执行").
    TimedOut { timeout_secs: u64 },
    /// The sender side was dropped before any verdict was sent
    /// (server crash, WS dropped, session reload, agent shutdown).
    ChannelClosed,
}

/// What to do with a Dangerous tool call when the caller wired **no**
/// approval channel (no approval requester / event sink).
///
/// This used to be a silent `Allow` fallback inside the pre-dispatch
/// gate. That made every caller without a socket — including the
/// network-reachable non-streaming HTTP route and the raw-text WS
/// branch — run `terminal` / CUA / `storage_clean` with no user
/// prompt. Callers now pass this explicitly so the exposure decision
/// is reviewable at each call site instead of being an implicit
/// property of "did you wire a socket".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalFallback {
    /// Deny the call. The safe default: use on any path a remote peer
    /// can reach. The caller cannot ask a human, so refusing and
    /// handing the LLM a machine-readable reason is correct.
    #[default]
    Deny,
    /// Allow the call. Only valid when the caller has no remote attack
    /// surface for this decision to be abused through — a local
    /// interactive stdio session, or a unit test.
    Allow,
}

impl ApprovalOutcome {
    /// Lightweight classifier used by the agent to decide whether to
    /// force a structured denial system note into the conversation
    /// context. Returns `true` for any non-Approved outcome.
    pub fn is_denial(&self) -> bool {
        !matches!(self, ApprovalOutcome::Approved)
    }

    /// Short label shown in logs, telemetry, and the frontend
    /// approval-outcome banner. Stable, lowercase, snake_case —
    /// changes here ripple to UI strings, so prefer additive variants.
    pub fn code(&self) -> &'static str {
        match self {
            ApprovalOutcome::Approved => "approved",
            ApprovalOutcome::DeniedByUser => "denied_by_user",
            ApprovalOutcome::DeniedWithFeedback { .. } => "denied_with_feedback",
            ApprovalOutcome::TimedOut { .. } => "timed_out",
            ApprovalOutcome::ChannelClosed => "channel_closed",
        }
    }

    /// Human-readable Chinese label used by the frontend banner.
    /// The English alias is intentionally omitted — this is a
    /// Chinese-first product, English labels live in `code()`.
    pub fn display_label_zh(&self) -> &'static str {
        match self {
            ApprovalOutcome::Approved => "已允许",
            ApprovalOutcome::DeniedByUser => "已拒绝",
            ApprovalOutcome::DeniedWithFeedback { .. } => "已拒绝（附说明）",
            ApprovalOutcome::TimedOut { .. } => "确认超时",
            ApprovalOutcome::ChannelClosed => "连接已断",
        }
    }
}

impl Default for ApprovalOutcome {
    /// `DeniedByUser` is the safest default — every other outcome is
    /// strictly more informative and the registry never spuriously
    /// approves a tool call we don't know about.
    fn default() -> Self {
        ApprovalOutcome::DeniedByUser
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_approved_is_non_denial() {
        assert!(!ApprovalOutcome::Approved.is_denial());
        assert!(ApprovalOutcome::DeniedByUser.is_denial());
        assert!(ApprovalOutcome::TimedOut { timeout_secs: 300 }.is_denial());
        assert!(ApprovalOutcome::ChannelClosed.is_denial());
    }

    #[test]
    fn codes_are_stable_strings() {
        // These strings are stable across the WS protocol — change
        // with care, the frontend banner maps them.
        assert_eq!(ApprovalOutcome::Approved.code(), "approved");
        assert_eq!(ApprovalOutcome::DeniedByUser.code(), "denied_by_user");
        assert_eq!(ApprovalOutcome::TimedOut { timeout_secs: 1 }.code(), "timed_out");
        assert_eq!(ApprovalOutcome::ChannelClosed.code(), "channel_closed");
    }

    #[test]
    fn default_is_denied_by_user() {
        assert_eq!(ApprovalOutcome::default(), ApprovalOutcome::DeniedByUser);
    }

    #[test]
    fn display_labels_are_zh_only() {
        // Banner shows Chinese; verify all variants have a label.
        for o in [
            ApprovalOutcome::Approved,
            ApprovalOutcome::DeniedByUser,
            ApprovalOutcome::TimedOut { timeout_secs: 300 },
            ApprovalOutcome::ChannelClosed,
        ] {
            assert!(!o.display_label_zh().is_empty());
        }
    }
}
