//! Real clarify backends.
//!
//! Two flavours:
//!
//! * [`SignalClarifyBackend`] — returns a JSON envelope immediately. Use in
//!   batch / scripted contexts where no real UI is available.
//! * [`ChannelClarifyBackend`] — blocks on a tokio oneshot until a paired
//!   [`ClarifyDispatcher`] responds. Use in the desktop app, TUI, and any
//!   context that actually has a user available to answer.

use std::collections::HashMap;
use hermix_config::env_compat;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use crate::tools::clarify::ClarifyBackend;
use hermix_core::ToolError;

/// Provides the current `session_id` so a [`ChannelClarifyBackend`] can
/// stamp each [`PendingClarify`] with the session it belongs to.
///
/// The agent loop pulls a fresh value at the moment `clarify` is called,
/// so session switches are reflected immediately.
pub trait ClarifySessionProvider: Send + Sync {
    fn current_session_id(&self) -> String;
}

/// Clarify backend that returns a JSON signal for the CLI/gateway layer
/// to present the question to the user. The actual UI interaction is
/// handled by the caller.
pub struct SignalClarifyBackend;

impl SignalClarifyBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SignalClarifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClarifyBackend for SignalClarifyBackend {
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        // Return a structured JSON response that signals the agent loop
        // to pause and ask the user for input.
        let result = json!({
            "type": "clarify_request",
            "question": question,
            "choices": choices.unwrap_or(&[]),
            "awaiting_response": true,
        });
        Ok(result.to_string())
    }
}

// ---------------------------------------------------------------------------
// ChannelClarifyBackend — blocks on oneshot, paired with ClarifyDispatcher
// ---------------------------------------------------------------------------

/// Default wait for a user response to a `clarify` call. Overridable via
/// `HERMES_CLARIFY_TIMEOUT_SECS`.
const DEFAULT_CLARIFY_TIMEOUT_SECS: u64 = 300;

/// [`ClarifyBackend`] that blocks the tool call on a tokio `oneshot`
/// receiver, letting the UI layer respond asynchronously.
///
/// The caller side owns a cloned [`ClarifyDispatcher`] which it uses to
/// fulfil the pending request (see [`ClarifyDispatcher::respond`]). If no
/// response arrives within the configured timeout, the tool returns an
/// `ExecutionFailed` error so the agent loop can retry or abandon.
pub struct ChannelClarifyBackend {
    dispatcher: ClarifyDispatcher,
    session_provider: Option<Arc<dyn ClarifySessionProvider>>,
}

impl ChannelClarifyBackend {
    pub fn new(dispatcher: ClarifyDispatcher) -> Self {
        Self {
            dispatcher,
            session_provider: None,
        }
    }

    /// Attach a session-id provider so each [`PendingClarify`] is stamped
    /// with the session it came from. The server uses this to route the
    /// `ClarifyRequest` event to the right WebSocket.
    pub fn with_session_provider(
        mut self,
        provider: Arc<dyn ClarifySessionProvider>,
    ) -> Self {
        self.session_provider = Some(provider);
        self
    }
}

#[async_trait]
impl ClarifyBackend for ChannelClarifyBackend {
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        let (tx, rx) = oneshot::channel::<String>();
        let request_id = format!("clarify-{}", uuid::Uuid::new_v4());
        let session_id = self
            .session_provider
            .as_ref()
            .map(|p| p.current_session_id())
            .unwrap_or_default();
        let req = PendingClarify {
            request_id: request_id.clone(),
            session_id,
            question: question.to_string(),
            choices: choices.map(|c| c.to_vec()).unwrap_or_default(),
            responder: tx,
        };

        self.dispatcher.enqueue(req).await;

        let wait_secs = env_compat("HERMIX_CLARIFY_TIMEOUT_SECS", "HERMES_CLARIFY_TIMEOUT_SECS")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CLARIFY_TIMEOUT_SECS);

        match timeout(Duration::from_secs(wait_secs), rx).await {
            Ok(Ok(answer)) => Ok(json!({
                "type": "clarify_response",
                "request_id": request_id,
                "question": question,
                "answer": answer,
            })
            .to_string()),
            Ok(Err(_)) => Err(ToolError::ExecutionFailed(
                "clarify responder dropped without answering".into(),
            )),
            Err(_) => Err(ToolError::ExecutionFailed(format!(
                "clarify timed out after {}s",
                wait_secs
            ))),
        }
    }
}

/// A pending clarify request awaiting a user response.
pub struct PendingClarify {
    /// Stable id so the UI can correlate the `clarify_response` with the
    /// outstanding question when several are queued.
    pub request_id: String,
    /// Owning session id — used by the server to route the
    /// `ClarifyRequest` event to the right WebSocket.
    pub session_id: String,
    pub question: String,
    pub choices: Vec<String>,
    pub(crate) responder: oneshot::Sender<String>,
}

impl PendingClarify {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Send the user's answer back to the agent tool call.
    pub fn respond(self, answer: impl Into<String>) -> Result<(), String> {
        self.responder
            .send(answer.into())
            .map_err(|_| "clarify backend was dropped".to_string())
    }
}

/// Cheap cloneable view of a pending request. The responder is *not*
/// included — resolution always goes through
/// [`ClarifyDispatcher::respond_by_id`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingClarifySnapshot {
    pub request_id: String,
    pub session_id: String,
    pub question: String,
    pub choices: Vec<String>,
}

impl PendingClarifySnapshot {
    pub fn from_pending(p: &PendingClarify) -> Self {
        Self {
            request_id: p.request_id.clone(),
            session_id: p.session_id.clone(),
            question: p.question.clone(),
            choices: p.choices.clone(),
        }
    }
}

/// Handle shared between the backend (producer) and the UI layer (consumer).
///
/// Internally it holds a `session_id → Vec<PendingClarify>` map plus a
/// `tokio::sync::Notify` so a UI subscriber can wake the moment a new
/// request is enqueued for its session. Producers use [`Self::enqueue`]
/// to push a request (the session id is carried on the
/// [`PendingClarify`] itself); the UI uses [`Self::take_next_for_session`]
/// to drain a single session's queue and [`Self::respond_by_id`] to
/// resolve a request by id.
#[derive(Clone)]
pub struct ClarifyDispatcher {
    inner: Arc<Mutex<HashMap<String, Vec<PendingClarify>>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for ClarifyDispatcher {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl ClarifyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn enqueue(&self, req: PendingClarify) {
        let session_id = req.session_id.clone();
        self.inner
            .lock()
            .await
            .entry(session_id)
            .or_default()
            .push(req);
        self.notify.notify_one();
    }

    /// Wait until the queue is non-empty, then return *all* pending
    /// requests across all sessions as snapshots. Resolution still
    /// happens via [`Self::respond_by_id`].
    pub async fn next_snapshot(&self) -> Vec<PendingClarifySnapshot> {
        loop {
            {
                let guard = self.inner.lock().await;
                let total: usize = guard.values().map(|v| v.len()).sum();
                if total > 0 {
                    return guard
                        .values()
                        .flat_map(|v| v.iter())
                        .map(PendingClarifySnapshot::from_pending)
                        .collect();
                }
            }
            self.notify.notified().await;
        }
    }

    /// Take (remove + return responder ownership) the next pending
    /// request for `session_id`. Returns `None` if the session has no
    /// pending requests.
    pub async fn take_next_for_session(
        &self,
        session_id: &str,
    ) -> Option<PendingClarify> {
        let mut guard = self.inner.lock().await;
        let queue = guard.get_mut(session_id)?;
        if queue.is_empty() {
            return None;
        }
        Some(queue.remove(0))
    }

    /// Look up a pending request by `request_id` (across all sessions),
    /// remove it from the queue, and complete the responder. Returns an
    /// error string if the id is unknown (already resolved, dropped, or
    /// never enqueued).
    pub async fn respond_by_id(
        &self,
        request_id: &str,
        answer: impl Into<String>,
    ) -> Result<(), String> {
        let answer = answer.into();
        let mut guard = self.inner.lock().await;
        for queue in guard.values_mut() {
            if let Some(pos) = queue.iter().position(|p| p.request_id == request_id) {
                let pending = queue.remove(pos);
                drop(guard);
                return pending
                    .responder
                    .send(answer)
                    .map_err(|_| "clarify backend was dropped".to_string());
            }
        }
        Err(format!("no pending clarify with id '{}'", request_id))
    }

    /// Total pending requests across all sessions. Diagnostic.
    pub async fn pending(&self) -> usize {
        self.inner.lock().await.values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatcher_roundtrip() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        std::env::set_var("HERMIX_CLARIFY_TIMEOUT_SECS", "5");

        let ask = tokio::spawn(async move {
            backend
                .ask("pick one?", Some(&["a".into(), "b".into()]))
                .await
        });

        // Give the producer a moment to enqueue.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let snapshots = dispatcher.next_snapshot().await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].question, "pick one?");
        dispatcher
            .respond_by_id(&snapshots[0].request_id, "a")
            .await
            .unwrap();

        let answer = ask.await.unwrap().unwrap();
        assert!(answer.contains("\"answer\":\"a\""));
        assert!(answer.contains("\"request_id\":"));
    }

    #[tokio::test]
    async fn dispatcher_times_out() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());
        std::env::set_var("HERMIX_CLARIFY_TIMEOUT_SECS", "1");

        let err = backend.ask("q?", None).await.unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
