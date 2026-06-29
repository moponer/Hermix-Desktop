//! Chrome DevTools Protocol wire format.
//!
//! Every CDP message is a JSON object. We model the three flavours
//! the client needs to demultiplex:
//!
//! 1. **Commands** the client sends: `{ id, method, params }`.
//!    `id` is a `u64` we allocate monotonically; `params` is
//!    method-specific (we keep it as `serde_json::Value`).
//! 2. **Responses** to a command: `{ id, result | error }`. We
//!    correlate to the in-flight command by `id` and route the
//!    payload back via a one-shot channel.
//! 3. **Events** the browser pushes unsolicited: `{ method, params }`.
//!    Most CDP verbs we use don't need events; the only one that
//!    matters today is `Page.loadEventFired` (we use it to wait for
//!    navigation to settle). Events are routed to per-method
//!    subscribers.
//!
//! Keep this file **free of `tokio`** — it's just serde shapes so
//! it stays testable without a runtime.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Command sent from client to browser.
///
/// `id == 0` is reserved for events (which carry no id); the
/// monotonic allocator in `CdpClient` skips it to avoid the
/// collision.
#[derive(Debug, Clone, Serialize)]
pub struct CdpCommand {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl CdpCommand {
    pub fn new(id: u64, method: impl Into<String>) -> Self {
        Self {
            id,
            method: method.into(),
            params: None,
        }
    }

    pub fn with_params(mut self, params: Value) -> Self {
        self.params = Some(params);
        self
    }
}

/// Response from browser. Exactly one of `result` or `error` is set.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpResponse {
    pub id: u64,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<CdpErrorBody>,
}

/// CDP error body — see
/// <https://chromedevtools.github.io/devtools-protocol/tot/#type-ErrorResponse>.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// Unsolicited event from browser.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpEvent {
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Raw frame from the wire — we discriminate by the presence of
/// `id` and `error`. Used to demultiplex a single read loop into
/// "this is a response" vs "this is an event".
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CdpEnvelope {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<CdpErrorBody>,
    #[serde(default)]
    pub params: Option<Value>,
}

impl CdpEnvelope {
    pub fn classify(&self) -> EnvelopeKind {
        if self.id.is_some() {
            EnvelopeKind::Response
        } else if self.method.is_some() {
            EnvelopeKind::Event
        } else {
            EnvelopeKind::Unknown
        }
    }
}

pub(crate) enum EnvelopeKind {
    Response,
    Event,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_serializes_with_optional_params() {
        let cmd = CdpCommand::new(7, "Page.navigate")
            .with_params(json!({ "url": "https://example.com" }));
        let s = serde_json::to_string(&cmd).unwrap();
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"method\":\"Page.navigate\""));
        assert!(s.contains("\"params\":{"));
    }

    #[test]
    fn command_serializes_without_params() {
        let cmd = CdpCommand::new(1, "Page.enable");
        let s = serde_json::to_string(&cmd).unwrap();
        assert!(!s.contains("\"params\""), "got: {s}");
    }

    #[test]
    fn response_classifies_as_response() {
        let env: CdpEnvelope =
            serde_json::from_str(r#"{"id":1,"result":{"frameId":"abc"}}"#).unwrap();
        assert!(matches!(env.classify(), EnvelopeKind::Response));
    }

    #[test]
    fn event_classifies_as_event() {
        let env: CdpEnvelope = serde_json::from_str(
            r#"{"method":"Page.loadEventFired","params":{"timestamp":1.2}}"#,
        )
        .unwrap();
        assert!(matches!(env.classify(), EnvelopeKind::Event));
    }

    #[test]
    fn error_body_deserializes() {
        let r: CdpResponse = serde_json::from_str(
            r#"{"id":3,"error":{"code":-32000,"message":"nav failed"}}"#,
        )
        .unwrap();
        let err = r.error.expect("error");
        assert_eq!(err.code, -32000);
        assert_eq!(err.message, "nav failed");
    }
}