//! WebSocket-backed CDP client.
//!
//! One [`CdpClient`] owns a single `WebSocketStream` plus a
//! monotonically-increasing command id. Each `call` allocates an
//! id, sends the command, and parks on a oneshot channel that the
//! background read loop wakes up when the matching response (or
//! error) arrives.
//!
//! Events (frames without an `id`) are forwarded to per-method
//! subscribers. Most callers don't need events; `Page.loadEventFired`
//! is the one we currently fan out for [`CdpClient::wait_for_load`].
//!
//! The client keeps a dedicated [`tokio::runtime::Runtime`]
//! (current-thread) so callers can use it from synchronous context
//! via [`CdpClient::call_blocking`]. This matches the rest of
//! hermix-cua's "sync + blocking" surface.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::manager::{CdpError, CdpResult};
use super::protocol::{CdpCommand, CdpEnvelope, CdpEvent, CdpResponse};
use crate::error::CuaError;

/// Self-hosted Chrome DevTools Protocol client. Wraps a single
/// WebSocket connection plus a tokio runtime so the public API is
/// synchronous.
pub struct CdpClient {
    rt: Runtime,
    inner: Arc<CdpClientInner>,
}

struct CdpClientInner {
    ws: Mutex<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    next_id: AtomicU64,
    /// Pending request/response correlation.
    pending: Mutex<HashMap<u64, oneshot::Sender<CdpResponse>>>,
    /// Per-method event subscribers. Only the methods we explicitly
    /// `subscribe()` to land here.
    events: Mutex<HashMap<String, mpsc::UnboundedSender<CdpEvent>>>,
    /// Suppress the "writer task" debug field — we keep the writer
    /// task handle so we can abort on drop.
    _writer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    _reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CdpClient {
    /// Open a WebSocket connection to Chrome's
    /// `webSocketDebuggerUrl` (typically
    /// `ws://127.0.0.1:9222/devtools/browser/<uuid>`).
    /// /// Spins up a dedicated `current_thread` tokio runtime and
    /// launches two background tasks: one that pumps frames from
    /// the socket into the pending/event maps, one that exposes
    /// the write half for `call` to push commands through.
    pub fn connect(ws_url: &str) -> CdpResult<Self> {
        let rt = Runtime::new().map_err(|e| CdpError::Protocol(format!(
            "build tokio runtime: {e}"
        )))?;

        // Open the socket. The future must be driven inside the
        // runtime; we use block_on to do that synchronously.
        let ws = rt.block_on(async {
            let (stream, _resp) = connect_async(ws_url)
                .await
                .map_err(|e| CdpError::Protocol(format!("WS connect {ws_url}: {e}")))?;
            Ok::<_, CdpError>(stream)
        })?;

        let inner = Arc::new(CdpClientInner {
            ws: Mutex::new(ws),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            _writer: Mutex::new(None),
            _reader: Mutex::new(None),
        });

        // Spawn the read loop on the runtime. We do this from
        // outside the runtime via `rt.spawn` because the runtime
        // was just built — there's no current-thread context yet
        // and `Handle::current()` would fail.
        let reader_inner = Arc::clone(&inner);
        let reader_handle = rt.spawn(reader_loop(reader_inner));

        {
            let mut guard = rt.block_on(async {
                let g = inner._reader.lock().await;
                Ok::<_, CdpError>(g)
            })?;
            *guard = Some(reader_handle);
        }

        Ok(Self { rt, inner })
    }

    /// Send a CDP command and block until the response arrives.
    /// `method` is e.g. `"Page.navigate"`. `params` is serialized
    /// to JSON.
    pub fn call_blocking(&self, method: &str, params: Value) -> CdpResult<Value> {
        self.rt.block_on(async { self.call(method, params).await })
    }

    /// Async form of [`CdpClient::call_blocking`]. Used by the
    /// `wait_for_load` helper which wants to interleave with
    /// event reads.
    pub async fn call(&self, method: &str, params: Value) -> CdpResult<Value> {
        // Reserve an id; skip 0 (reserved for events).
        let id = loop {
            let candidate = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            if candidate != 0 {
                break candidate;
            }
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().await;
            pending.insert(id, tx);
        }

        let cmd = CdpCommand::new(id, method).with_params(params);
        let payload = serde_json::to_string(&cmd)
            .map_err(|e| CdpError::Protocol(format!("encode {method}: {e}")))?;

        // Send through the same ws (tungstenite lets multiple
        // tasks share via Mutex).
        let send_res = {
            let mut ws = self.inner.ws.lock().await;
            ws.send(Message::Text(payload)).await
        };
        if let Err(e) = send_res {
            // Clean up the pending entry so a future retry can
            // claim a new id cleanly.
            self.inner.pending.lock().await.remove(&id);
            return Err(CdpError::Protocol(format!("WS send {method}: {e}")));
        }

        // Wait for the response.
        let resp = rx
            .await
            .map_err(|_| CdpError::Protocol(format!("{method} cancelled")))?;

        match (resp.result, resp.error) {
            (Some(v), None) => Ok(v),
            (None, Some(err)) => Err(CdpError::Protocol(format!(
                "CDP {}: code={} message={}",
                method, err.code, err.message
            ))),
            (None, None) => Ok(Value::Null),
            (Some(_), Some(err)) => Err(CdpError::Protocol(format!(
                "CDP {method}: both result and error set (error={})",
                err.message
            ))),
        }
    }

    /// Subscribe to events with a given method name (e.g.
    /// `"Page.loadEventFired"`). Returns an `mpsc::UnboundedReceiver`
    /// the caller can poll. We use unbounded because CDP events
    /// are sparse (a few per page lifecycle); backpressure would
    /// stall the read loop and stall *all* commands.
    pub async fn subscribe(&self, method: &str) -> CdpResult<mpsc::UnboundedReceiver<CdpEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner
            .events
            .lock()
            .await
            .insert(method.to_string(), tx);
        Ok(rx)
    }

    /// Convenience: subscribe to `Page.loadEventFired` and wait up
    /// to `timeout` for at least one event.
    pub async fn wait_for_load(&self, timeout: Duration) -> CdpResult<()> {
        let mut rx = self.subscribe("Page.loadEventFired").await?;
        tokio::time::timeout(timeout, async move {
            rx.recv().await;
        })
        .await
        .map_err(|_| CdpError::Timeout("wait_for_load timed out".into()))?;
        Ok(())
    }

    /// Reference to the dedicated tokio runtime. Only used by
    /// [`BrowserHandle`] for short async helpers that need to be
    /// driven synchronously.
    pub(crate) fn rt_ref(&self) -> &Runtime {
        &self.rt
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        // Closing the WebSocket cleanly is best-effort. The
        // tungstenite sink will EOF on drop. We don't try to
        // await anything here because we're in Drop.
    }
}

/// Background task: read frames forever, demultiplex into the
/// pending map (responses) or the events map (events).
async fn reader_loop(inner: Arc<CdpClientInner>) {
    loop {
        let frame = {
            let mut ws = inner.ws.lock().await;
            ws.next().await
        };
        match frame {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<CdpEnvelope>(&text) {
                Ok(env) => match env.classify() {
                    super::protocol::EnvelopeKind::Response => {
                        let id = env.id.unwrap_or(0);
                        let resp = CdpResponse {
                            id,
                            result: env.result,
                            error: env.error,
                        };
                        let sender = inner.pending.lock().await.remove(&id);
                        if let Some(tx) = sender {
                            // If the receiver was dropped, ignore —
                            // caller gave up.
                            let _ = tx.send(resp);
                        } else {
                            tracing::debug!(
                                "CDP response for unknown id {id}; dropping"
                            );
                        }
                    }
                    super::protocol::EnvelopeKind::Event => {
                        let method = env.method.unwrap_or_default();
                        let event = CdpEvent {
                            method: method.clone(),
                            params: env.params,
                        };
                        let sender = inner.events.lock().await.get(&method).cloned();
                        if let Some(tx) = sender {
                            let _ = tx.send(event);
                        }
                    }
                    super::protocol::EnvelopeKind::Unknown => {
                        tracing::debug!("CDP envelope with neither id nor method: {text}");
                    }
                },
                Err(e) => {
                    tracing::warn!("CDP frame parse error: {e}; payload={text}");
                }
            },
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {
                // Ping / Pong / Binary — ignore. (CDP only uses
                // text frames; tungstenite handles ping/pong
                // transparently.)
            }
            Some(Err(e)) => {
                tracing::warn!("CDP read error: {e}");
                break;
            }
        }
    }
    // Fail any pending callers so they don't hang forever.
    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, tx) in pending {
        let _ = tx.send(CdpResponse {
            id: 0,
            result: None,
            error: Some(super::protocol::CdpErrorBody {
                code: -1,
                message: "ws disconnected".into(),
                data: None,
            }),
        });
    }
}

// ---------------------------------------------------------------------------
// Convert internal CdpError → public CuaError at the boundary.
// ---------------------------------------------------------------------------

impl From<CdpError> for CuaError {
    fn from(e: CdpError) -> Self {
        CuaError::Cdp(format!("{e:?}"))
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for CdpError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        CdpError::Protocol(format!("ws: {e}"))
    }
}

#[allow(dead_code)]
pub(crate) fn ok_json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_skips_zero() {
        // We can't easily build a CdpClient without a real socket,
        // so this just exercises the id generator indirectly via a
        // simulated AtomicU64.
        let a = AtomicU64::new(0);
        let id = loop {
            let candidate = a.fetch_add(1, Ordering::Relaxed);
            if candidate != 0 {
                break candidate;
            }
        };
        assert_eq!(id, 1);
    }

    #[test]
    fn envelope_classify_unknown_when_empty() {
        let env: CdpEnvelope = serde_json::from_str("{}").unwrap();
        assert!(matches!(
            env.classify(),
            super::super::protocol::EnvelopeKind::Unknown
        ));
    }
}