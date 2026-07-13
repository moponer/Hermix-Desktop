//! Per-loop message bookkeeping cache.
//!
//! Phase 5 (2026-07-03): the 2 fields (`last_checkpoint_messages`,
//! `last_content_with_tools`) that used to live inline on `LoopState`
//! were extracted into one struct so `publish_from` and the helpers
//! in `process_post_response` don't have to pass them as individual
//! `&mut Option<...>` arguments.
//!
//! Physically split out of `agent_loop.rs` (2026-07-04); API contract
//! unchanged. Fields stay `pub(crate)` so existing
//! `state.message_cache.last_content_with_tools` test-code
//! assertions keep compiling without accessor-method churn.

use hermix_core::Message;

/// Per-loop message bookkeeping cache.
///
/// The two fields stay co-located on `self.message_cache` and mutate
/// in place via the `store_*` / `take_*` / `checkpoint` methods.
/// Reset / `Default` produce the same all-`None` state as before,
/// so `LoopState::default()` semantics are unchanged.
#[derive(Debug, Clone, Default)]
pub struct MessageCache {
    /// Last checkpoint messages for crash recovery / auto-rollback.
    pub(crate) last_checkpoint_messages: Option<Vec<Message>>,
    /// Last assistant message that included `tool_use` (for the
    /// codex-ack continuation logic on the next turn).
    pub(crate) last_content_with_tools: Option<String>,
}

impl MessageCache {
    /// Store a fresh checkpoint snapshot (taken from the context after
    /// a successful turn). Overwrites any prior checkpoint.
    pub(crate) fn store_checkpoint(&mut self, snapshot: Vec<Message>) {
        self.last_checkpoint_messages = Some(snapshot);
    }

    /// Overwrite the cached assistant content (used by
    /// `process_post_response` and `prepare_assistant_turn` when the
    /// assistant's last visible text came bundled with tool calls).
    pub(crate) fn store_content_with_tools(&mut self, content: Option<String>) {
        self.last_content_with_tools = content;
    }

    /// Take the cached assistant content out (used by the finalize
    /// path when handing off to the next session / persisting the
    /// agent result). Leaves `None` behind.
    #[allow(dead_code)] // P1-3: production caller removed; kept for tests + future use
    pub(crate) fn take_content_with_tools(&mut self) -> Option<String> {
        self.last_content_with_tools.take()
    }

    /// Borrow the checkpoint for rollback / publish — caller clones
    /// if it needs ownership.
    pub(crate) fn checkpoint(&self) -> Option<&Vec<Message>> {
        self.last_checkpoint_messages.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_none() {
        let c = MessageCache::default();
        assert!(c.checkpoint().is_none());
        assert!(c.last_content_with_tools.is_none());
    }

    #[test]
    fn store_and_borrow_checkpoint() {
        let mut c = MessageCache::default();
        let snap = Vec::<Message>::new();
        c.store_checkpoint(snap.clone());
        assert_eq!(c.checkpoint().unwrap(), &snap);
        // store overwrites prior
        let snap2 = vec![Message::user("hi")];
        c.store_checkpoint(snap2.clone());
        assert_eq!(c.checkpoint().unwrap(), &snap2);
    }

    #[test]
    fn store_and_take_content_with_tools_clears_after_take() {
        let mut c = MessageCache::default();
        c.store_content_with_tools(Some("hello".into()));
        assert_eq!(c.last_content_with_tools.as_deref(), Some("hello"));
        let taken = c.take_content_with_tools();
        assert_eq!(taken.as_deref(), Some("hello"));
        assert!(c.last_content_with_tools.is_none());
    }

    #[test]
    fn store_content_with_tools_none_clears_without_take() {
        let mut c = MessageCache::default();
        c.store_content_with_tools(Some("hello".into()));
        c.store_content_with_tools(None);
        assert!(c.last_content_with_tools.is_none());
    }
}
