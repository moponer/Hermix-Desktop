//! LLM-based speaker selection — analyzes conversation context and
//! participant descriptions to pick the most suitable next speaker.
//!
//! Mirrors AutoGen's `SelectorGroupChat` pattern.

use crate::orchestrator::{GroupChatMessage, GroupChatParticipant};

/// LLM-based speaker selection. Requires an LLM call to decide who
/// should speak next based on the conversation context.
pub struct SelectorGroupChat {
    /// Whether to allow the same speaker to be selected consecutively.
    pub allow_repeated_speaker: bool,
    /// Custom prompt template for the selection LLM call.
    pub selector_prompt: Option<String>,
}

impl SelectorGroupChat {
    pub fn new() -> Self {
        Self {
            allow_repeated_speaker: false,
            selector_prompt: None,
        }
    }

    /// Return the selection prompt that asks the LLM to choose the
    /// next speaker. The caller sends this as a lightweight LLM call
    /// and returns the selected participant name.
    pub fn build_selection_prompt(
        &self,
        participants: &[GroupChatParticipant],
        history: &[GroupChatMessage],
        last_speaker: Option<&str>,
        task: &str,
    ) -> String {
        let mut prompt = String::from("You are the group chat manager. Select the next participant to speak.\n\n");
        prompt.push_str(&format!("Task: {}\n\n", task));
        prompt.push_str("Participants:\n");
        for (i, p) in participants.iter().enumerate() {
            prompt.push_str(&format!("{}. {} — {}\n", i + 1, p.name, p.description));
        }
        prompt.push_str("\nConversation so far:\n");
        if history.is_empty() {
            prompt.push_str("  (no messages yet)\n");
        } else {
            for msg in history.iter().rev().take(10).rev() {
                prompt.push_str(&format!("  {}: {}\n", msg.sender, msg.content));
            }
        }
        if let Some(last) = last_speaker {
            if !self.allow_repeated_speaker {
                prompt.push_str(&format!("\nThe last speaker was {}. Do NOT select the same person again.\n", last));
            }
        }
        prompt.push_str("\nReply with ONLY the participant name (exactly as listed above).");
        prompt
    }

    /// Parse the LLM's response to extract the selected participant name.
    /// Returns the index into the participants slice.
    pub fn parse_selection(
        &self,
        llm_reply: &str,
        participants: &[GroupChatParticipant],
    ) -> Option<usize> {
        let reply = llm_reply.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        // Try exact match first.
        if let Some(idx) = participants.iter().position(|p| p.name == reply) {
            return Some(idx);
        }
        // Try case-insensitive match.
        let reply_lower = reply.to_lowercase();
        if let Some(idx) = participants.iter().position(|p| p.name.to_lowercase() == reply_lower) {
            return Some(idx);
        }
        // Try number match (e.g. "1" for first participant).
        if let Ok(num) = reply.parse::<usize>() {
            if num >= 1 && num <= participants.len() {
                return Some(num - 1);
            }
        }
        None
    }
}
