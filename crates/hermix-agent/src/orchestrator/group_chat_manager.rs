//! AutoGen-style GroupChatManager — orchestrates multi-agent conversations.
//!
//! Receives a task, selects the next speaker via a configurable strategy,
//! broadcasts messages to all participants, and checks termination conditions.

use crate::orchestrator::round_robin::RoundRobinGroupChat;
use crate::orchestrator::selector::SelectorGroupChat;
use crate::orchestrator::termination::TerminationCondition;
use crate::orchestrator::{GroupChatMessage, GroupChatParticipant, GroupChatTurnResult};

/// Speaker selection strategy for the group chat.
pub enum SpeakerSelection {
    /// Agents speak in fixed order.
    RoundRobin(RoundRobinGroupChat),
    /// LLM dynamically selects the next speaker.
    Selector(SelectorGroupChat),
}

/// The core group chat orchestrator.
///
/// Usage:
/// ```ignore
/// let mut manager = GroupChatManager::new(participants, task, strategy, terminations);
/// loop {
///     match manager.next_turn().await {
///         GroupChatTurnResult::Message(msg) => broadcast(msg),
///         GroupChatTurnResult::Terminated(reason) => break,
///     }
/// }
/// ```
pub struct GroupChatManager {
    /// Participants in this group chat.
    participants: Vec<GroupChatParticipant>,
    /// The original task given to the group.
    task: String,
    /// Speaker selection strategy.
    strategy: SpeakerSelection,
    /// Termination conditions — any one met ends the chat.
    terminations: Vec<TerminationCondition>,
    /// Conversation history.
    history: Vec<GroupChatMessage>,
    /// Name of the last speaker (for de-duplication).
    last_speaker: Option<String>,
    /// Turn counter.
    turn_count: usize,
}

impl GroupChatManager {
    /// Create a new group chat manager.
    pub fn new(
        participants: Vec<GroupChatParticipant>,
        task: String,
        strategy: SpeakerSelection,
        terminations: Vec<TerminationCondition>,
    ) -> Self {
        Self {
            participants,
            task,
            strategy,
            terminations,
            history: Vec::new(),
            last_speaker: None,
            turn_count: 0,
        }
    }

    /// Select the next participant to speak based on the configured strategy.
    fn select_next_speaker(&mut self) -> Option<usize> {
        match &mut self.strategy {
            SpeakerSelection::RoundRobin(rr) => {
                rr.select_next(&self.participants, &self.history, self.last_speaker.as_deref())
            }
            SpeakerSelection::Selector(ref sel) => {
                let _prompt = sel.build_selection_prompt(
                    &self.participants,
                    &self.history,
                    self.last_speaker.as_deref(),
                    &self.task,
                );
                // Caller must provide the LLM selection result via
                // `apply_selection()`.
                None
            }
        }
    }

    /// For SelectorGroupChat: the caller provides the LLM's chosen
    /// participant name, and this function applies it and returns
    /// the next turn result.
    pub fn apply_selection(&mut self, llm_selection: &str) -> GroupChatTurnResult {
        // Check termination first.
        if let Some(reason) = self.check_termination() {
            return GroupChatTurnResult::Terminated(reason);
        }

        match &self.strategy {
            SpeakerSelection::Selector(ref sel) => {
                if let Some(idx) = sel.parse_selection(llm_selection, &self.participants) {
                    self.last_speaker = Some(self.participants[idx].name.clone());
                    // Return a signal to the caller indicating which
                    // participant should speak next. The actual message
                    // content is filled in by `record_message()`.
                    GroupChatTurnResult::Message(GroupChatMessage {
                        sender: self.participants[idx].name.clone(),
                        content: String::new(),
                    })
                } else {
                    GroupChatTurnResult::Terminated(format!("Unknown participant: {}", llm_selection))
                }
            }
            _ => GroupChatTurnResult::Terminated("apply_selection called on non-Selector strategy".into()),
        }
    }

    /// Record a participant's message and advance the turn.
    pub fn record_message(&mut self, sender: &str, content: String) {
        self.history.push(GroupChatMessage {
            sender: sender.to_string(),
            content,
        });
        self.turn_count += 1;
        self.last_speaker = Some(sender.to_string());
    }

    /// Execute one turn of round-robin group chat (no LLM call needed).
    pub fn next_round_robin(&mut self) -> GroupChatTurnResult {
        if let Some(reason) = self.check_termination() {
            return GroupChatTurnResult::Terminated(reason);
        }
        match self.select_next_speaker() {
            Some(idx) => {
                let name = self.participants[idx].name.clone();
                self.last_speaker = Some(name.clone());
                GroupChatTurnResult::Message(GroupChatMessage {
                    sender: name,
                    content: String::new(),
                })
            }
            None => GroupChatTurnResult::Terminated("no available speakers".into()),
        }
    }

    /// Check all termination conditions. Returns Some(reason) if any met.
    fn check_termination(&self) -> Option<String> {
        for tc in &self.terminations {
            if tc.is_met(&self.history, self.turn_count) {
                return Some(tc.description());
            }
        }
        None
    }

    // ── Accessors ──

    pub fn participants(&self) -> &[GroupChatParticipant] { &self.participants }
    pub fn history(&self) -> &[GroupChatMessage] { &self.history }
    pub fn task(&self) -> &str { &self.task }
    pub fn turn_count(&self) -> usize { self.turn_count }
}
