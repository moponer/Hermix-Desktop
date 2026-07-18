//! Handoff primitive — borrowed from openai-agents-python's Handoff concept.
//!
//! In openai-agents-python, a **Handoff** is when one agent transfers
//! control of the conversation to another agent. The original agent exits;
//! the target agent takes over until it hands back or the task completes.
//!
//! This is distinct from **Agents-as-Tools** (where agent A calls agent B
//! as a tool and retains control). The group-chat scheduler already covers
//! the agents-as-tools case via `SchedulerAction::Dispatch` — the leader
//! dispatches subtasks and collects results. This module adds the missing
//! **handoff** path: the leader can transfer the whole conversation to a
//! specialist, who then answers the user directly.
//!
//! ## When to handoff vs delegate
//!
//! | Situation | Action |
//! |-----------|--------|
//! | User asks a broad question needing multiple specialists | Delegate (subtasks) |
//! | User asks a deep domain question one specialist can fully answer | Handoff (Transfer) |
//! | Leader can answer directly | Neither — just respond |

use serde::{Deserialize, Serialize};

/// A resolved handoff directive ready for the runtime to execute.
///
/// Built from [`super::agent_contract::HandoffDirective`] after validating
/// that the target agent exists in the room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedHandoff {
    /// The agent_id gaining control.
    pub target_agent_id: String,
    /// Why the handoff happened (shown to the user as a separator).
    pub reason: String,
    /// Whether this is a permanent transfer or a temporary delegation.
    pub kind: HandoffKind,
}

/// The flavor of handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffKind {
    /// The target agent takes over the conversation. The leader steps back
    /// until the target hands back or the task completes.
    ///
    /// NOTE (M9): the control transfer is currently **per-turn** — the
    /// runtime does not yet persist a durable "active controller", so the
    /// next user message still routes to `__leader__`. This is a known
    /// limitation, not a bug; a full takeover would require recording the
    /// controller on the room and routing subsequent dispatches to it.
    /// See REVIEW_REPORT_2026-07-18.md (M9).
    Transfer,
    /// A bounded delegation — the target answers one question, then control
    /// returns to the leader. (Agents-as-tools pattern.)
    Delegate,
}

impl ResolvedHandoff {
    /// Build a human-readable separator label for the chat UI.
    ///
    /// Rendered as a centered divider when the handoff happens so the user
    /// can see the conversation changed hands.
    pub fn separator_label(&self, target_display_name: &str) -> String {
        match self.kind {
            HandoffKind::Transfer => {
                format!("已转交 {} 接管：{}", target_display_name, self.reason)
            }
            HandoffKind::Delegate => {
                format!("委托 {} 协助：{}", target_display_name, self.reason)
            }
        }
    }
}

/// Validate that a handoff target exists in the room's agent roster.
///
/// Returns `Some(ResolvedHandoff)` if the target is a real member, or
/// `None` if the target agent_id is not in the room (the runtime treats
/// this as a no-op and the leader keeps control).
pub fn resolve_handoff(
    directive: &super::agent_contract::HandoffDirective,
    room_agents: &[super::persistence::AgentRoomProfile],
) -> Option<ResolvedHandoff> {
    use super::agent_contract::HandoffDirective;
    let (target_id, reason, kind) = match directive {
        HandoffDirective::Transfer { agent_id, reason } => {
            (agent_id.clone(), reason.clone(), HandoffKind::Transfer)
        }
        HandoffDirective::Delegate { agent_id, task } => {
            (agent_id.clone(), task.clone(), HandoffKind::Delegate)
        }
    };
    // The leader cannot hand off to itself, and the target must be a real
    // member of the room (not the built-in `__leader__`).
    if target_id == super::persistence::LEADER_AGENT_ID {
        return None;
    }
    let exists = room_agents
        .iter()
        .any(|a| a.agent_id == target_id && a.role != super::persistence::AgentRole::Leader);
    if !exists {
        return None;
    }
    Some(ResolvedHandoff {
        target_agent_id: target_id,
        reason,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::persistence::{AgentRole, AgentRoomProfile};

    fn make_agent(id: &str, role: AgentRole) -> AgentRoomProfile {
        AgentRoomProfile {
            agent_id: id.into(),
            room_id: "r1".into(),
            role,
            session_id: "s1".into(),
            model_override: None,
            display_name: None,
        }
    }

    #[test]
    fn resolve_transfer_to_real_member() {
        let agents = vec![
            make_agent("__leader__", AgentRole::Leader),
            make_agent("alice", AgentRole::Member),
        ];
        let directive = super::super::agent_contract::HandoffDirective::Transfer {
            agent_id: "alice".into(),
            reason: "legal question".into(),
        };
        let resolved = resolve_handoff(&directive, &agents).unwrap();
        assert_eq!(resolved.target_agent_id, "alice");
        assert_eq!(resolved.kind, HandoffKind::Transfer);
        assert!(resolved.separator_label("Alice").contains("转交"));
    }

    #[test]
    fn resolve_transfer_to_nonexistent_returns_none() {
        let agents = vec![make_agent("alice", AgentRole::Member)];
        let directive = super::super::agent_contract::HandoffDirective::Transfer {
            agent_id: "ghost".into(),
            reason: "nope".into(),
        };
        assert!(resolve_handoff(&directive, &agents).is_none());
    }

    #[test]
    fn resolve_transfer_to_leader_returns_none() {
        let agents = vec![
            make_agent("__leader__", AgentRole::Leader),
            make_agent("alice", AgentRole::Member),
        ];
        let directive = super::super::agent_contract::HandoffDirective::Transfer {
            agent_id: "__leader__".into(),
            reason: "self".into(),
        };
        assert!(resolve_handoff(&directive, &agents).is_none());
    }

    #[test]
    fn delegate_separator_uses_delegate_label() {
        let resolved = ResolvedHandoff {
            target_agent_id: "bob".into(),
            reason: "quick question".into(),
            kind: HandoffKind::Delegate,
        };
        let label = resolved.separator_label("Bob");
        assert!(label.contains("委托"));
        assert!(label.contains("Bob"));
    }
}
