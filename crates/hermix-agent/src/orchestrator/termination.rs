//! Termination conditions for group chat — determines when a group
//! chat session should end.

use crate::orchestrator::GroupChatMessage;

/// A condition that, when met, terminates the group chat.
#[derive(Clone)]
pub enum TerminationCondition {
    /// Maximum number of messages before termination.
    MaxMessages(usize),
    /// Stop when a message from any participant contains this keyword.
    Keyword(String),
    /// Stop after a maximum number of turns (each turn = one speaker).
    MaxTurns(usize),
    /// Custom condition: function that checks messages.
    Custom(Arc<dyn Fn(&[GroupChatMessage]) -> bool + Send + Sync>),
}

impl TerminationCondition {
    /// Check whether the termination condition is met.
    pub fn is_met(&self, history: &[GroupChatMessage], turn_count: usize) -> bool {
        match self {
            TerminationCondition::MaxMessages(limit) => history.len() >= *limit,
            TerminationCondition::Keyword(ref kw) => {
                history.iter().any(|m| m.content.contains(kw.as_str()))
            }
            TerminationCondition::MaxTurns(limit) => turn_count >= *limit,
            TerminationCondition::Custom(ref f) => f(history),
        }
    }

    /// Human-readable description of this condition.
    pub fn description(&self) -> String {
        match self {
            TerminationCondition::MaxMessages(n) => format!("max {} messages", n),
            TerminationCondition::Keyword(ref k) => format!("keyword '{}' appears", k),
            TerminationCondition::MaxTurns(n) => format!("max {} turns", n),
            TerminationCondition::Custom(_) => "custom condition".into(),
        }
    }
}

use std::sync::Arc;
