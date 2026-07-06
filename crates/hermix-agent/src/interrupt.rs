//! Interrupt mechanism for the agent loop.
//!
//! Provides a thread-safe interrupt controller that allows external threads
//! (TUI input handler, message receiver, Ctrl+C signal handler) to request
//! the agent to stop its current tool-calling loop and optionally redirect
//! to a new message.
//!
//! Corresponds to Python `run_agent.py`'s `interrupt()` / `clear_interrupt()`
//! / `_interrupt_requested` system.
//!
//! # Cancellation primitive (O1.2)
//!
//! Beyond the polling-style `check_interrupt()`, the controller exposes a
//! `wait_for_interrupt()` future. Callers that are inside a long-running
//! async operation (e.g. a 30 s+ LLM API call) can `tokio::select!` between
//! their work and `wait_for_interrupt()` so the agent reacts to Ctrl+C
//! immediately instead of waiting for the HTTP timeout.
//!
//! ```rust,ignore
//! tokio::select! {
//!     result = long_running_call(…) => result,
//!     _ = interrupt.wait_for_interrupt() => Err(AgentError::Interrupted { message: None }),
//! }
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermix_core::AgentError;

/// Thread-safe interrupt controller for the agent loop.
///
/// Clone-friendly: all clones share the same underlying state via `Arc`.
///
/// # Usage
///
/// ```rust,ignore
/// let ctrl = InterruptController::new();
///
/// // From another thread (e.g. Ctrl+C handler):
/// ctrl.interrupt(Some("New user message".into()));
///
/// // In the agent loop, before each LLM call / tool execution:
/// ctrl.check_interrupt()?;
///
/// // During a long-running async op:
/// tokio::select! {
///     res = llm_call(…) => res,
///     _ = ctrl.wait_for_interrupt() => Err(AgentError::Interrupted { message: None }),
/// }
/// ```
#[derive(Debug, Clone)]
pub struct InterruptController {
    /// Atomic flag — set to `true` when an interrupt is requested.
    flag: Arc<AtomicBool>,
    /// Optional redirect message that triggered the interrupt.
    redirect_message: Arc<Mutex<Option<String>>>,
    /// Tokio notification primitive for async cancellation (O1.2).
    /// `notify_one()` fires when `interrupt()` is called; `notified()`
    /// resolves immediately if an interrupt was already requested.
    notify: Arc<tokio::sync::Notify>,
}

impl InterruptController {
    /// Create a new interrupt controller in the non-interrupted state.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            redirect_message: Arc::new(Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Request the agent to interrupt its current loop.
    ///
    /// Optionally provide a redirect message (e.g. new user input that
    /// should be processed instead of continuing the current tool chain).
    ///
    /// Fires the async notification so any `tokio::select!` racing on
    /// [`wait_for_interrupt`](Self::wait_for_interrupt) unblocks immediately.
    pub fn interrupt(&self, message: Option<String>) {
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = message;
        }
        // Set the flag AFTER storing the message so readers always see
        // a consistent (flag=true, message=Some) pair.
        self.flag.store(true, Ordering::Release);
        // Wake any async waiter (O1.2).
        self.notify.notify_one();
    }

    /// Clear any pending interrupt request.
    pub fn clear_interrupt(&self) {
        self.flag.store(false, Ordering::Release);
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = None;
        }
    }

    /// Check whether an interrupt has been requested.
    pub fn is_interrupted(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// If an interrupt is pending, consume it (clear flag + redirect) and return `Some(redirect)`.
    ///
    /// Used by the agent loop for **graceful** shutdown with
    /// [`hermix_core::AgentResult::interrupted`] instead of
    /// [`AgentError::Interrupted`]. Call sites that still need the legacy error
    /// should use [`Self::check_interrupt`].
    pub fn take_interrupt_graceful(&self) -> Option<Option<String>> {
        if !self.is_interrupted() {
            return None;
        }
        let msg = self.take_redirect_message();
        self.clear_interrupt();
        Some(msg)
    }

    /// Take the redirect message (if any), leaving `None` in its place.
    pub fn take_redirect_message(&self) -> Option<String> {
        if let Ok(mut guard) = self.redirect_message.lock() {
            guard.take()
        } else {
            None
        }
    }

    /// Check the interrupt flag and return `Err(AgentError::Interrupted)`
    /// if set. This is the primary check point used inside the agent loop
    /// before each LLM call and tool execution.
    pub fn check_interrupt(&self) -> Result<(), AgentError> {
        if self.is_interrupted() {
            let message = self.take_redirect_message();
            Err(AgentError::Interrupted { message })
        } else {
            Ok(())
        }
    }

    /// Async-friendly cancellation future (O1.2).
    ///
    /// Returns a future that resolves when `interrupt()` is called.
    /// If an interrupt was already requested, the future resolves
    /// immediately on the next poll. Callers use this in a
    /// `tokio::select!` to cancel long-running async work (LLM API
    /// calls, tool execution) without waiting for a polling window.
    ///
    /// ```rust,ignore
    /// tokio::select! {
    ///     res = llm_call(...) => res,
    ///     _ = interrupt.wait_for_interrupt() => {
    ///         interrupt.check_interrupt() // consume the flag, return error
    ///     }
    /// }
    /// ```
    pub async fn wait_for_interrupt(&self) {
        // If already interrupted, resolve immediately.
        if self.is_interrupted() {
            return;
        }
        self.notify.notified().await;
    }
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_controller_is_not_interrupted() {
        let ctrl = InterruptController::new();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_interrupt_sets_flag() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(None);
        assert!(ctrl.is_interrupted());
    }

    #[test]
    fn test_interrupt_with_message() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("new input".into()));
        assert!(ctrl.is_interrupted());

        match ctrl.check_interrupt() {
            Err(AgentError::Interrupted { message }) => {
                assert_eq!(message.as_deref(), Some("new input"));
            }
            other => panic!("expected Interrupted, got {:?}", other),
        }
    }

    #[test]
    fn test_clear_interrupt() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("msg".into()));
        assert!(ctrl.is_interrupted());

        ctrl.clear_interrupt();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_take_redirect_message_consumes() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("hello".into()));

        let msg = ctrl.take_redirect_message();
        assert_eq!(msg.as_deref(), Some("hello"));

        // Second take returns None
        let msg2 = ctrl.take_redirect_message();
        assert!(msg2.is_none());
    }

    #[test]
    fn test_clone_shares_state() {
        let ctrl1 = InterruptController::new();
        let ctrl2 = ctrl1.clone();

        ctrl1.interrupt(Some("shared".into()));
        assert!(ctrl2.is_interrupted());
    }
}
