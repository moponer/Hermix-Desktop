//! Phase 14 / H3 — consciousness stream API.
//!
//! The desktop UI renders the agent's runtime vitals as a full-screen
//! ECG (electrocardiogram) line. This module exposes two surfaces:
//!
//! - **REST** — `GET /api/consciousness/recent?limit=N` returns the
//!   last N samples (oldest first); `GET /api/consciousness/summary`
//!   returns the current state + vitals snapshot. Both endpoints are
//!   useful for the "rewind" affordance and for tests.
//!
//! - **WebSocket** — `GET /ws/consciousness` upgrades to a
//!   broadcast subscription. The server forwards every sample the
//!   agent emits; the UI animates the wave as samples arrive.
//!
//! Design notes:
//! - The tracker is owned by `HttpServerState` (one per process). We
//!   intentionally don't pool or shard — the broadcast channel handles
//!   multiple subscribers natively.
//! - The WS handler is read-only: clients receive samples but cannot
//!   emit. The agent loop writes; the UI reads. There's no need for
//!   a duplex channel.
//! - The REST handler caps `limit` at the ring capacity (1024) so
//!   a runaway client can't try to drain more than we buffer.

use axum::extract::ws::{Message as WsMessage, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use futures::SinkExt;
use futures::StreamExt;

use hermix_agent::ConsciousnessTracker;

use crate::HttpServerState;

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// `GET /ws/consciousness` — upgrades to a WebSocket that streams
/// samples as they arrive. The first message after upgrade is always
/// a JSON snapshot so the client has a baseline; subsequent messages
/// are individual samples.
/// /// Message format (JSON, one per WS text frame):
/// ```json
/// { "kind": "snapshot", "state": "...", "vitals": {...}, "total_emitted": 0 }
/// ```
/// or
/// ```json
/// { "kind": "sample", "sample": { ...ConsciousnessSample... } }
/// ```
pub async fn ws_consciousness(
    ws: WebSocketUpgrade,
    State(state): State<HttpServerState>,
) -> Response {
    let tracker = state.consciousness_tracker.clone();
    ws.on_upgrade(move |socket| handle_consciousness_socket(socket, tracker))
}

/// Loop: send initial snapshot, then forward samples until the
/// client closes the connection or the broadcast channel lags.
async fn handle_consciousness_socket(socket: WebSocket, tracker: ConsciousnessTracker) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = tracker.subscribe();

    // Send the initial snapshot so the client has a baseline before
    // the first sample arrives.
    let (current_state, vitals) = tracker.snapshot();
    let total = tracker.recent(usize::MAX).len();
    let snapshot = serde_json::json!({
        "kind": "snapshot",
        "state": current_state,
        "vitals": vitals,
        "total_emitted": total,
    });
    if let Ok(text) = serde_json::to_string(&snapshot) {
        if sender.send(WsMessage::Text(text.into())).await.is_err() {
            return;
        }
    }

    // Forward samples until the client disconnects or we hit a send
    // error. We deliberately ignore `RecvError::Lagged` — the UI
    // should just keep painting from the next sample rather than
    // try to backfill a gap.
    loop {
        tokio::select! {
            sample = rx.recv() => {
                match sample {
                    Ok(s) => {
                        let msg = serde_json::json!({
                            "kind": "sample",
                            "sample": s,
                        });
                        match serde_json::to_string(&msg) {
                            Ok(text) => {
                                if sender.send(WsMessage::Text(text.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                // Serialization shouldn't fail for our
                                // types, but if it does, skip the sample
                                // rather than kill the connection.
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Tell the client how many we skipped so its
                        // own counters stay sane.
                        let msg = serde_json::json!({
                            "kind": "lagged",
                            "skipped": n,
                        });
                        if let Ok(text) = serde_json::to_string(&msg) {
                            if sender.send(WsMessage::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            // Read side: we don't expect inbound messages, but if the
            // client pings or closes we need to react so the loop
            // doesn't leak.
            inbound = receiver.next() => {
                match inbound {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    // Ignore pings/text — read-only channel.
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

