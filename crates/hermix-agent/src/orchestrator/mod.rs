//! Multi-agent chat orchestration (AutoGen-inspired).
//!
//! Provides group chat patterns where multiple agent participants share
//! a conversation thread, with a manager deciding who speaks next.
//!
//! ## Patterns
//!
//! - [`GroupChatManager`] — core orchestrator: receives messages, selects
//!   the next speaker via a configurable strategy, and broadcasts results.
//! - [`SelectorGroupChat`] — LLM-based speaker selection: analyzes the
//!   conversation context and each participant's `description` to pick
//!   the most suitable next agent.
//! - [`RoundRobinGroupChat`] — fixed round-robin order (A→B→C→A→...).
//! - [`TerminationCondition`] — stop when a condition is met (max messages,
//!   keyword, function).

pub mod group_chat_manager;
pub mod selector;
pub mod round_robin;
pub mod termination;

pub use group_chat_manager::GroupChatManager;
pub use selector::SelectorGroupChat;
pub use round_robin::RoundRobinGroupChat;
pub use termination::TerminationCondition;

/// A participant in a group chat — either a human or an AI agent.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupChatParticipant {
    /// Unique name (shown in chat as @name).
    pub name: String,
    /// Role description used by SelectorGroupChat to decide when this
    /// participant should speak (e.g. "You are a Python expert. Answer
    /// coding questions concisely.").
    pub description: String,
    /// Whether this participant is a human (true) or an AI agent (false).
    pub is_human: bool,
}

impl GroupChatParticipant {
    pub fn new(name: impl Into<String>, description: impl Into<String>, is_human: bool) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            is_human,
        }
    }
}

/// A single message within a group chat session.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupChatMessage {
    /// The participant who sent this message.
    pub sender: String,
    /// Message content.
    pub content: String,
}

/// Result from a group chat turn: either a message from a participant,
/// or a termination signal.
#[derive(Debug, PartialEq)]
pub enum GroupChatTurnResult {
    Message(GroupChatMessage),
    Terminated(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_participant_creation() {
        let p = GroupChatParticipant::new("alice", "Python expert", false);
        assert_eq!(p.name, "alice");
        assert_eq!(p.description, "Python expert");
        assert!(!p.is_human);
    }

    #[test]
    fn test_round_robin_selects_agents_skipping_humans() {
        let mut rr = round_robin::RoundRobinGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("human1", "User", true),
            GroupChatParticipant::new("bot1", "Assistant A", false),
            GroupChatParticipant::new("human2", "Observer", true),
            GroupChatParticipant::new("bot2", "Assistant B", false),
        ];
        // Should skip human1 and human2, select bot1 then bot2
        let idx1 = rr.select_next(&participants, &[], None);
        assert_eq!(idx1, Some(1)); // bot1
        let idx2 = rr.select_next(&participants, &[], None);
        assert_eq!(idx2, Some(3)); // bot2
        let idx3 = rr.select_next(&participants, &[], None);
        assert_eq!(idx3, Some(1)); // bot1 again (wraps around)
    }

    #[test]
    fn test_round_robin_all_humans_returns_none() {
        let mut rr = round_robin::RoundRobinGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("alice", "User", true),
            GroupChatParticipant::new("bob", "User", true),
        ];
        assert_eq!(rr.select_next(&participants, &[], None), None);
    }

    #[test]
    fn test_round_robin_empty_participants() {
        let mut rr = round_robin::RoundRobinGroupChat::new();
        assert_eq!(rr.select_next(&[], &[], None), None);
    }

    #[test]
    fn test_selector_builds_prompt_with_participants_and_history() {
        let sel = selector::SelectorGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("alice", "Python expert", false),
            GroupChatParticipant::new("bob", "Designer", false),
        ];
        let history = vec![
            GroupChatMessage { sender: "alice".into(), content: "I can write the code".into() },
        ];
        let prompt = sel.build_selection_prompt(&participants, &history, Some("alice"), "Build a web app");
        assert!(prompt.contains("alice"));
        assert!(prompt.contains("bob"));
        assert!(prompt.contains("Python expert"));
        assert!(prompt.contains("Designer"));
        assert!(prompt.contains("Build a web app"));
        assert!(prompt.contains("I can write the code"));
        assert!(prompt.contains("Do NOT select the same person again"));
    }

    #[test]
    fn test_selector_parse_selection_exact_match() {
        let sel = selector::SelectorGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("alice", "", false),
            GroupChatParticipant::new("bob", "", false),
        ];
        assert_eq!(sel.parse_selection("alice", &participants), Some(0));
        assert_eq!(sel.parse_selection("bob", &participants), Some(1));
    }

    #[test]
    fn test_selector_parse_selection_case_insensitive() {
        let sel = selector::SelectorGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("Alice", "", false),
        ];
        assert_eq!(sel.parse_selection("alice", &participants), Some(0));
        assert_eq!(sel.parse_selection("ALICE", &participants), Some(0));
    }

    #[test]
    fn test_selector_parse_selection_numeric() {
        let sel = selector::SelectorGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("alice", "", false),
            GroupChatParticipant::new("bob", "", false),
        ];
        assert_eq!(sel.parse_selection("1", &participants), Some(0));
        assert_eq!(sel.parse_selection("2", &participants), Some(1));
    }

    #[test]
    fn test_selector_parse_selection_unknown_returns_none() {
        let sel = selector::SelectorGroupChat::new();
        let participants = vec![
            GroupChatParticipant::new("alice", "", false),
        ];
        assert_eq!(sel.parse_selection("charlie", &participants), None);
        assert_eq!(sel.parse_selection("0", &participants), None);  // zero-indexed, 0 is invalid
        assert_eq!(sel.parse_selection("99", &participants), None); // out of range
    }

    #[test]
    fn test_termination_max_messages() {
        let tc = TerminationCondition::MaxMessages(3);
        let history = vec![
            GroupChatMessage { sender: "a".into(), content: "1".into() },
            GroupChatMessage { sender: "b".into(), content: "2".into() },
        ];
        assert!(!tc.is_met(&history, 0));
        let history = vec![
            GroupChatMessage { sender: "a".into(), content: "1".into() },
            GroupChatMessage { sender: "b".into(), content: "2".into() },
            GroupChatMessage { sender: "c".into(), content: "3".into() },
        ];
        assert!(tc.is_met(&history, 0));
    }

    #[test]
    fn test_termination_keyword() {
        let tc = TerminationCondition::Keyword("APPROVED".into());
        let history = vec![
            GroupChatMessage { sender: "a".into(), content: "I think this is done".into() },
        ];
        assert!(!tc.is_met(&history, 0));
        let history = vec![
            GroupChatMessage { sender: "b".into(), content: "APPROVED".into() },
        ];
        assert!(tc.is_met(&history, 0));
    }

    #[test]
    fn test_termination_max_turns() {
        let tc = TerminationCondition::MaxTurns(5);
        assert!(!tc.is_met(&[], 3));
        assert!(tc.is_met(&[], 5));
        assert!(tc.is_met(&[], 10));
    }

    #[test]
    fn test_group_chat_manager_round_robin_flow() {
        use crate::orchestrator::group_chat_manager::SpeakerSelection;
        use crate::orchestrator::round_robin::RoundRobinGroupChat;

        let participants = vec![
            GroupChatParticipant::new("bot1", "Helper A", false),
            GroupChatParticipant::new("bot2", "Helper B", false),
        ];
        let mut manager = GroupChatManager::new(
            participants,
            "Test task".into(),
            SpeakerSelection::RoundRobin(RoundRobinGroupChat::new()),
            vec![TerminationCondition::MaxTurns(4)],
        );

        // Turn 1: bot1 speaks
        match manager.next_round_robin() {
            GroupChatTurnResult::Message(msg) => {
                assert_eq!(msg.sender, "bot1");
                manager.record_message("bot1", "Hello from bot1".into());
            }
            _ => panic!("expected message"),
        }
        assert_eq!(manager.turn_count(), 1);

        // Turn 2: bot2 speaks
        match manager.next_round_robin() {
            GroupChatTurnResult::Message(msg) => {
                assert_eq!(msg.sender, "bot2");
                manager.record_message("bot2", "Hello from bot2".into());
            }
            _ => panic!("expected message"),
        }
        assert_eq!(manager.turn_count(), 2);

        // Turn 3: bot1 again
        match manager.next_round_robin() {
            GroupChatTurnResult::Message(msg) => {
                assert_eq!(msg.sender, "bot1");
                manager.record_message("bot1", "Back again".into());
            }
            _ => panic!("expected message"),
        }

        // Turn 4: triggers MaxTurns(4) termination
        match manager.next_round_robin() {
            GroupChatTurnResult::Message(msg) => {
                manager.record_message("bot2", "Last msg".into());
            }
            _ => panic!("expected message"),
        }

        // Turn 5 should be terminated (MaxTurns reached)
        assert!(matches!(manager.next_round_robin(), GroupChatTurnResult::Terminated(_)));
    }

    #[test]
    fn test_group_chat_manager_selector_flow() {
        use crate::orchestrator::group_chat_manager::SpeakerSelection;

        let participants = vec![
            GroupChatParticipant::new("alice", "Python expert", false),
            GroupChatParticipant::new("bob", "Designer", false),
        ];
        let selector = SelectorGroupChat::new();
        let mut manager = GroupChatManager::new(
            participants,
            "Build a web app".into(),
            SpeakerSelection::Selector(selector),
            vec![TerminationCondition::MaxMessages(3)],
        );

        // Apply selection: alice speaks first
        match manager.apply_selection("alice") {
            GroupChatTurnResult::Message(msg) => {
                assert_eq!(msg.sender, "alice");
                manager.record_message("alice", "I'll write the backend".into());
            }
            other => panic!("expected message, got {:?}", other),
        }
        assert_eq!(manager.turn_count(), 1);

        // Apply selection: bob speaks
        match manager.apply_selection("bob") {
            GroupChatTurnResult::Message(msg) => {
                assert_eq!(msg.sender, "bob");
                manager.record_message("bob", "I'll design the UI".into());
            }
            other => panic!("expected message, got {:?}", other),
        }
        assert_eq!(manager.turn_count(), 2);
        assert_eq!(manager.history().len(), 2);

        // Invalid selection
        match manager.apply_selection("unknown") {
            GroupChatTurnResult::Terminated(reason) => {
                assert!(reason.contains("unknown"));
            }
            other => panic!("expected terminated, got {:?}", other),
        }
    }

    #[test]
    fn test_group_chat_manager_history_accessors() {
        use crate::orchestrator::group_chat_manager::SpeakerSelection;

        let participants = vec![
            GroupChatParticipant::new("bot1", "Helper", false),
        ];
        let mut manager = GroupChatManager::new(
            participants.clone(),
            "test".into(),
            SpeakerSelection::RoundRobin(RoundRobinGroupChat::new()),
            vec![],
        );

        assert_eq!(manager.participants().len(), 1);
        assert_eq!(manager.participants()[0].name, "bot1");
        assert_eq!(manager.task(), "test");
        assert_eq!(manager.turn_count(), 0);
        assert!(manager.history().is_empty());
    }
}
