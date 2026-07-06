//! Round-robin speaker selection — participants speak in fixed order.

use crate::orchestrator::{GroupChatMessage, GroupChatParticipant};

/// Round-robin speaker selection: cycles through participants in order,
/// skipping humans (they speak when they want).
pub struct RoundRobinGroupChat {
    /// Index into the participants slice for the next expected speaker.
    next_index: usize,
}

impl RoundRobinGroupChat {
    pub fn new() -> Self {
        Self { next_index: 0 }
    }

    /// Given the conversation history and participants, pick the next
    /// speaker. Skips human participants.
    pub fn select_next(
        &mut self,
        participants: &[GroupChatParticipant],
        _history: &[GroupChatMessage],
        _last_speaker: Option<&str>,
    ) -> Option<usize> {
        if participants.is_empty() {
            return None;
        }
        // Try each participant starting from next_index, wrap around.
        for offset in 0..participants.len() {
            let idx = (self.next_index + offset) % participants.len();
            // Skip human participants in auto-mode.
            if !participants[idx].is_human {
                self.next_index = (idx + 1) % participants.len();
                return Some(idx);
            }
        }
        // All participants are human — no auto-speaker.
        None
    }
}
