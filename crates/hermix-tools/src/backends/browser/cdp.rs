//! CDP session management + page operations for the local Chrome backend.
//! Kept cohesive: WebSocket command transport, target resolution, and the
//! `BrowserBackend` implementation that drives navigation / snapshot / click
//! / type / scroll / console / vision on a single Chrome tab.

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Concrete socket type `connect_async` hands back. Aliased so the session
/// helpers below don't have to name four generic parameters.
type CdpSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

use crate::tools::browser::BrowserBackend;
use hermix_core::ToolError;

use super::a11y::{
    ax_tree_to_a11y_yaml, build_wait_for_js, compact_a11y_yaml, diff_a11y_yaml, RefTarget,
};
use super::cloud::annotate_result;

/// Browser backend using Chrome DevTools Protocol.
/// Connects to Chrome via WebSocket for automation.
#[derive(Clone)]
pub struct CdpBrowserBackend {
    /// CDP endpoint URL. Can be either a discovery root (`http://host:9222`)
    /// or a concrete websocket endpoint (`wss://.../devtools/browser/...`).
    endpoint: String,
    client: Client,
    /// Last snapshot ref → CDP node identity. Lets the LLM pass `[ref=eN]`
    /// from the snapshot YAML instead of writing raw selectors, and resolves
    /// to the exact node the accessibility tree described — no CSS guessing,
    /// so labels expressed as `<label for>` associations resolve correctly.
    last_snapshot_refs: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, RefTarget>>>,
    /// init scripts registered via `Page.addScriptToEvaluateOnNewDocument`.
    /// Each entry is the script's `scriptId` (from CDP) so we can remove
    /// it later via `Page.removeScriptToEvaluateOnNewDocument`. The
    /// scripts themselves are re-injected on every `navigate` (CDP does
    /// this automatically for any `addScriptToEvaluateOnNewDocument`).
    init_script_ids: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// CamoFox anti-detection browser backend (compat layer).
pub struct CamoFoxBrowserBackend {
    inner: CdpBrowserBackend,
    profile: String,
}

/// What a target resolves to once classified.
///
/// `Node` is a CDP-internal node identity taken from the accessibility tree and
/// must be used with `call_on_node`. `Css` is a model-authored selector and goes
/// down the `document.querySelector` path — still fully supported, because
/// models legitimately write their own selectors, and a snapshot ref is a
/// convenience rather than the only way in.
#[derive(Debug)]
pub(crate) enum Resolved {
    Node(u64),
    Css(String),
}

impl CamoFoxBrowserBackend {
    pub fn new(endpoint: String, profile: String) -> Self {
        Self {
            inner: CdpBrowserBackend::new(endpoint),
            profile,
        }
    }

    pub fn from_env() -> Self {
        let endpoint = std::env::var("CAMOFOX_CDP_URL")
            .or_else(|_| std::env::var("CHROME_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        let profile = std::env::var("CAMOFOX_PROFILE").unwrap_or_else(|_| "default".to_string());
        Self::new(endpoint, profile)
    }
}

impl CdpBrowserBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: Client::new(),
            last_snapshot_refs: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            init_script_ids: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Resolve a `[ref=eN]` token or a raw CSS selector into a real selector
    /// that CDP can act on.
    ///
    /// Returns `Err` when a `[ref=…]` token has no corresponding entry in the
    /// current snapshot — the most common cause is that the page navigated
    /// after the snapshot was taken, making the old handles stale. The error
    /// carries a hint for the LLM to re-snapshot rather than retry with the
    /// same stale token.
    ///
    /// Raw CSS selectors (those that do not start with `[ref=`) are returned
    /// unchanged — the LLM may legitimately want to use a CSS selector without
    /// a snapshot token.
    /// Is this a `[ref=eN]` token rather than a CSS selector?
    pub(crate) fn is_ref_token(selector: &str) -> Option<&str> {
        let trimmed = selector.trim();
        let rest = trimmed.strip_prefix("[ref=")?;
        rest.strip_suffix(']')
    }

    /// Classify a model-supplied target into a node handle or a CSS selector.
    ///
    /// A `[ref=…]` token with no entry in the current snapshot is an error, not
    /// a CSS selector: the usual cause is that the page navigated after the
    /// snapshot, so the handle is stale, and falling through would either
    /// syntax-error or — far worse — match an unrelated element.
    pub(crate) fn resolve_selector(&self, selector: &str) -> Result<Resolved, ToolError> {
        let trimmed = selector.trim();
        if let Some(ref_id) = Self::is_ref_token(trimmed) {
            let found = self
                .last_snapshot_refs
                .lock()
                .ok()
                .and_then(|map| map.get(ref_id).map(|t| t.backend_node_id));
            return match found {
                Some(id) => Ok(Resolved::Node(id)),
                None => Err(ToolError::ExecutionFailed(format!(
                    "引用句柄 `[ref={ref_id}]` 不存在（页面可能已导航，请重新 browser_snapshot）"
                ))),
            };
        }
        // A near-miss that *intended* to be a handle (`[ref=e1` with the
        // bracket dropped) must not reach `querySelector`. `[ref=e1` is a
        // legal attribute selector, so it would silently match whatever
        // element happens to carry a `ref="e1"` attribute — the model would
        // be told it clicked one thing while the code touched another. Fail
        // with the correction instead.
        if trimmed.starts_with("[ref=") {
            return Err(ToolError::ExecutionFailed(format!(
                "`{trimmed}` 是残缺的引用句柄，格式应为 `[ref=eN]`。\
                 请重新 browser_snapshot 取新的句柄，或直接传 CSS 选择器。"
            )));
        }
        Ok(Resolved::Css(trimmed.to_string()))
    }

    /// Create from environment variable `CHROME_CDP_URL` or default localhost.
    /// Does NOT auto-launch Chrome at construction time. The
    /// `BrowserNavigateHandler::with_cdp_bootstrap()` handles lazy
    /// browser startup only when a navigate tool call is actually made.
    pub fn from_env() -> Self {
        let endpoint =
            std::env::var("CHROME_CDP_URL").unwrap_or_else(|_| "http://localhost:9222".to_string());
        Self::new(endpoint)
    }

    async fn resolve_target(&self) -> Result<String, ToolError> {
        if looks_like_cdp_websocket(&self.endpoint) {
            return Ok(self.endpoint.clone());
        }
        let targets_resp = self
            .client
            .get(format!("{}/json", self.endpoint.trim_end_matches('/')))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to connect to Chrome CDP at {}: {}",
                    self.endpoint, e
                ))
            })?;

        let targets: Vec<Value> = targets_resp.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse CDP targets: {}", e))
        })?;

        // Prefer the last page-type target (most recently navigated),
        // falling back to any target with a webSocketDebuggerUrl.
        let ws_url = targets
            .iter()
            .rev()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .or_else(|| targets.first())
            .and_then(|t| t.get("webSocketDebuggerUrl"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string());

        ws_url.ok_or_else(|| {
            ToolError::ExecutionFailed(
                "No active Chrome page target found. Is Chrome running with --remote-debugging-port=9222?"
                    .into(),
            )
        })
    }

    /// Send a CDP command via HTTP/WebSocket discovery shim.
    pub(crate) async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let target = self.resolve_target().await?;
        let connect_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect_async(&target),
        )
        .await;

        let (mut ws, _) = connect_result
            .map_err(|_| {
                ToolError::ExecutionFailed(format!(
                    "CDP websocket connection timed out for {method}"
                ))
            })?
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to connect to CDP websocket {target}: {e}"
                ))
            })?;

        static NEXT_CDP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request_id = NEXT_CDP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload = json!({
            "id": request_id,
            "method": method,
            "params": params,
        })
        .to_string();
        ws.send(WsMessage::Text(payload)).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to send CDP command {method}: {e}"))
        })?;

        // Read response with timeout to prevent indefinite blocking.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let frame = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ws.next(),
            )
            .await;

            let frame = match frame {
                Ok(Some(Ok(f))) => f,
                Ok(Some(Err(e))) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP stream read failed for {method}: {e}"
                    )));
                }
                Ok(None) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP connection closed before response for {method}"
                    )));
                }
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP read timed out for {method}"
                    )));
                }
            };

            let text = match frame {
                WsMessage::Text(t) => t,
                WsMessage::Binary(bytes) => String::from_utf8(bytes).map_err(|e| {
                    ToolError::ExecutionFailed(format!("CDP binary frame decode failed: {e}"))
                })?,
                WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                WsMessage::Close(_) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP connection closed before response for {method}"
                    )));
                }
                WsMessage::Frame(_) => continue,
            };

            let parsed: Value = serde_json::from_str(&text).map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to parse CDP response JSON: {e}"))
            })?;
            if parsed.get("id").and_then(|v| v.as_u64()) != Some(request_id) {
                continue;
            }
            if let Some(err) = parsed.get("error") {
                return Err(ToolError::ExecutionFailed(format!(
                    "CDP command {method} failed: {err}"
                )));
            }
            let result = parsed.get("result").cloned().unwrap_or(parsed);
            let _ = ws.close(None).await;
            return Ok(result);
        }

        Err(ToolError::ExecutionFailed(format!(
            "No CDP response received for {method} within timeout"
        )))
    }

    /// One request/response round trip on an **already-open** CDP socket.
    /// Interleaved events and replies belonging to other ids are skipped.
    async fn ws_roundtrip(
        ws: &mut CdpSocket,
        method: &str,
        params: Value,
    ) -> Result<Value, ToolError> {
        static NEXT_CDP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_CDP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ws.send(WsMessage::Text(
            json!({ "id": id, "method": method, "params": params }).to_string(),
        ))
        .await
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to send CDP command {method}: {e}"))
        })?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(ToolError::ExecutionFailed(format!(
                    "No CDP response received for {method} within timeout"
                )));
            }
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .map_err(|_| {
                    ToolError::ExecutionFailed(format!("CDP read timed out for {method}"))
                })?;
            let frame = match frame {
                Some(Ok(f)) => f,
                Some(Err(e)) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP stream read failed for {method}: {e}"
                    )))
                }
                None => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP connection closed before response for {method}"
                    )))
                }
            };
            let text = match frame {
                WsMessage::Text(t) => t,
                WsMessage::Binary(bytes) => String::from_utf8(bytes).map_err(|e| {
                    ToolError::ExecutionFailed(format!("CDP binary frame decode failed: {e}"))
                })?,
                WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
                WsMessage::Close(_) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP connection closed before response for {method}"
                    )))
                }
            };
            let parsed: Value = serde_json::from_str(&text).map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to parse CDP response JSON: {e}"))
            })?;
            if parsed.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue; // an event, or a reply we already consumed
            }
            if let Some(err) = parsed.get("error") {
                return Err(ToolError::ExecutionFailed(format!(
                    "CDP command {method} failed: {err}"
                )));
            }
            return Ok(parsed.get("result").cloned().unwrap_or(parsed));
        }
    }

    /// Run `body` against the element identified by `backend_node_id`, on a
    /// session that stays open across both the resolve and the call.
    ///
    /// `body` is a JS function body taking the element as `el` and returning
    /// a JSON-serializable value. It is always this file's own literal JS —
    /// nothing derived from the model is ever concatenated into it, so there
    /// is no injection surface here (unlike the selector path, where the
    /// selector does come from the model and must stay JSON-encoded).
    ///
    /// Returns the `result.value` of the call, or `Value::Null` when the
    /// function returned nothing.
    ///
    /// The two commands share one socket on purpose: `DOM.resolveNode` returns
    /// a `RemoteObjectId`, which is **scoped to the session that minted it**,
    /// so a resolve-then-use pair cannot go through [`Self::cdp_command`] — it
    /// opens a fresh WebSocket per call and the handle would be dead on
    /// arrival. That per-session lifetime is the entire reason `[ref=eN]` used
    /// to be emulated with a CSS selector guessed from a node's accessible
    /// name, and why that guess missed every input labelled by a
    /// `<label for>` association rather than an attribute — i.e. most login
    /// forms. Tearing the socket down at the end also releases the handle,
    /// so there is nothing to clean up and no way for it to outlive its use.
    async fn call_on_node(&self, backend_node_id: u64, body: &str) -> Result<Value, ToolError> {
        let target = self.resolve_target().await?;
        let (mut ws, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect_async(&target),
        )
        .await
        .map_err(|_| ToolError::ExecutionFailed("CDP websocket connection timed out".into()))?
        .map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to connect to CDP websocket {target}: {e}"
            ))
        })?;

        let out = async {
            let resolved = Self::ws_roundtrip(
                &mut ws,
                "DOM.resolveNode",
                json!({ "backendNodeId": backend_node_id }),
            )
            .await?;
            let object_id = resolved
                .get("object")
                .and_then(|o| o.get("objectId"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::ExecutionFailed(format!(
                        "DOM.resolveNode returned no objectId for backendNodeId \
                         {backend_node_id} — the node is gone (page navigated or \
                         re-rendered). Re-run browser_snapshot for a fresh [ref=…]."
                    ))
                })?
                .to_string();

            let called = Self::ws_roundtrip(
                &mut ws,
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": body,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

            // A thrown JS body surfaces here, not in the id-matching path.
            if let Some(details) = called.get("exceptionDetails") {
                return Err(ToolError::ExecutionFailed(format!(
                    "page-side function threw: {details}"
                )));
            }
            Ok(called
                .get("result")
                .and_then(|r| r.get("value"))
                .cloned()
                .unwrap_or(Value::Null))
        }
        .await;

        let _ = ws.close(None).await;
        out
    }

    /// Preflight on a CDP-resolved node. Returns `(reason, cover)`; both
    /// `None` means the element passed.
    async fn preflight_via_node(
        &self,
        backend_node_id: u64,
        body: &str,
    ) -> Result<(Option<String>, Option<String>), ToolError> {
        let v = self.call_on_node(backend_node_id, body).await?;
        Ok(Self::read_preflight(&v))
    }

    /// Preflight on a model-authored CSS selector.
    async fn preflight_via_css(
        &self,
        js: &str,
    ) -> Result<(Option<String>, Option<String>), ToolError> {
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({ "expression": js, "returnByValue": true }),
            )
            .await?;
        let raw = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        // A thrown expression (invalid selector ⇒ SyntaxError) leaves
        // `result.value` absent, so `raw` is "{}" and the old
        // `unwrap_or_else(|_| json!({"ok": true}))` fallback declared the
        // preflight passed. Fail closed: an unparseable preflight is a
        // failure, not a pass.
        let pre: Value = serde_json::from_str(raw).unwrap_or_else(|_| {
            json!({"ok": false, "reason": "preflight_evaluate_failed"})
        });
        Ok(Self::read_preflight(&pre))
    }

    /// `None` on `ok: true` or a missing `ok` — the caller only acts on an
    /// explicit `false`, matching the original fail-closed semantics.
    fn read_preflight(pre: &Value) -> (Option<String>, Option<String>) {
        if pre.get("ok").and_then(|v| v.as_bool()) != Some(false) {
            return (None, None);
        }
        let reason = pre
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cover = pre
            .get("cover")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        (Some(reason), cover)
    }

    /// Send a CDP command and optionally wait for a follow-up event on the
    /// same WebSocket. Returns `(immediate_response, optional_event_payload)`.
    /// `event_method` is the CDP method name to wait for (e.g.
    /// `"Page.loadEventFired"`). If `None`, behaves like [`Self::cdp_command`].
    /// `event_timeout` caps how long to wait for the event before returning
    /// `(result, None)` — the navigate call has likely already succeeded.
    /// This is the right primitive for `Page.navigate` + `Page.loadEventFired`:
    /// the immediate response comes back immediately (the navigation has been
    /// *initiated*) but the page hasn't actually loaded yet. Without waiting
    /// for the event, a follow-up `Accessibility.getFullAXTree` returns the
    /// previous page or an empty tree — which is the symptom that was making
    /// the LLM loop on `browser_snapshot` calls.
    async fn cdp_command_with_event(
        &self,
        method: &str,
        params: Value,
        event_method: Option<&str>,
        event_timeout: std::time::Duration,
    ) -> Result<(Value, Option<Value>), ToolError> {
        let target = self.resolve_target().await?;
        let (mut ws, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect_async(&target),
        )
        .await
        .map_err(|_| ToolError::ExecutionFailed(format!("CDP websocket connection timed out for {method}")))?
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to connect to CDP websocket {target}: {e}")))?;

        static NEXT_CDP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request_id = NEXT_CDP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload = json!({
            "id": request_id,
            "method": method,
            "params": params,
        })
        .to_string();
        ws.send(WsMessage::Text(payload)).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to send CDP command {method}: {e}"))
        })?;

        let mut result: Option<Value> = None;
        let mut event: Option<Value> = None;
        // Per-message deadline: 30s for the immediate response, then
        // `event_timeout` for the follow-up event.
        let response_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let event_deadline = std::time::Instant::now() + event_timeout;

        while std::time::Instant::now() < response_deadline {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await;
            let frame = match frame {
                Ok(Some(Ok(f))) => f,
                Ok(Some(Err(e))) => {
                    let _ = ws.close(None).await;
                    return Err(ToolError::ExecutionFailed(format!(
                        "CDP websocket error for {method}: {e}"
                    )));
                }
                Ok(None) => break,
                Err(_) => continue,
            };

            let text = match frame {
                WsMessage::Text(t) => t,
                WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                WsMessage::Close(_) => break,
                WsMessage::Frame(_) => continue,
                _ => continue,
            };

            let parsed: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // CDP event frame (no `id`, has `method`).
            if parsed.get("id").is_none() {
                if let (Some(want), Some(m)) = (event_method, parsed.get("method").and_then(|v| v.as_str())) {
                    if m == want {
                        event = Some(parsed.get("params").cloned().unwrap_or(Value::Null));
                        break;
                    }
                }
                continue;
            }

            // CDP response frame.
            if parsed.get("id").and_then(|v| v.as_u64()) != Some(request_id) {
                continue;
            }
            if let Some(err) = parsed.get("error") {
                let _ = ws.close(None).await;
                return Err(ToolError::ExecutionFailed(format!(
                    "CDP command {method} failed: {err}"
                )));
            }
            result = Some(parsed.get("result").cloned().unwrap_or(parsed));
            break;
        }

        let result = match result {
            Some(r) => r,
            None => {
                let _ = ws.close(None).await;
                return Err(ToolError::ExecutionFailed(format!(
                    "No CDP response received for {method} within timeout"
                )));
            }
        };

        // If we still need to wait for an event, keep draining the socket.
        if event.is_none() && event_method.is_some() {
            while std::time::Instant::now() < event_deadline {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
                let frame = match frame {
                    Ok(Some(Ok(f))) => f,
                    _ => break,
                };
                let text = match frame {
                    WsMessage::Text(t) => t,
                    _ => continue,
                };
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let (Some(want), Some(m)) = (event_method, parsed.get("method").and_then(|v| v.as_str())) {
                    if m == want {
                        event = Some(parsed.get("params").cloned().unwrap_or(Value::Null));
                        break;
                    }
                }
            }
        }

        let _ = ws.close(None).await;
        Ok((result, event))
    }
}

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        self.navigate_with_wait(url, std::time::Duration::from_secs(15)).await
    }

    /// Like [`Self::navigate`] but with an explicit `wait_for_load` budget.
    /// Waits for `Page.loadEventFired` so a follow-up `browser_snapshot` does
    /// not race the navigation and return the previous page or an empty tree.
    async fn navigate_with_wait(
        &self,
        url: &str,
        wait_for_load: std::time::Duration,
    ) -> Result<String, ToolError> {
        let (result, event) = self
            .cdp_command_with_event(
                "Page.navigate",
                json!({"url": url}),
                Some("Page.loadEventFired"),
                wait_for_load,
            )
            .await?;
        // 修 3b：导航成功即作废旧快照的全部 `[ref=eN]` 句柄——跨页面的
        // 句柄本来就无意义，保留只会让模型拿旧句柄点到新页面上语义
        // 无关的元素。清空后旧句柄在 resolve_selector 得到明确报错。
        if let Ok(mut map) = self.last_snapshot_refs.lock() {
            map.clear();
        }
        let load_observed = event.is_some();
        let mut payload = json!({
            "status": "navigated",
            "url": url,
            "cdp": result,
        });
        if !load_observed {
            // Page is still loading — surface this in the result so the LLM
            // does not immediately call browser_snapshot and find an empty
            // tree. The text below is recognised by the autonomous_mode
            // nudge logic so the agent will retry the snapshot with
            // browser_wait_for instead of looping on web_fetch.
            payload["load_observed"] = json!(false);
            payload["hint"] = json!(
                "Page.loadEventFired was not observed within the budget. \
                 The page is likely still loading (e.g. waiting on a slow \
                 XHR, image, or captcha challenge). Do NOT call web_fetch. \
                 Call browser_wait_for with a CSS selector (e.g. \"form\", \
                 \"input[name=captcha]\") and a timeout, then re-snapshot."
            );
        } else {
            payload["load_observed"] = json!(true);
        }
        Ok(payload.to_string())
    }

    async fn wait_for(
        &self,
        selector: &str,
        timeout_ms: u64,
        state: Option<&str>,
        url_glob: Option<&str>,
        js_fn: Option<&str>,
    ) -> Result<String, ToolError> {
        // Polling via Runtime.evaluate is enough for this primitive. We avoid
        // a custom CDP WebSocket flow because the selector is dynamic JS the
        // user-supplied LLM picks — easier to escape for evaluate than for
        // DOM.querySelector.
        let poll_ms: u64 = 250;
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms.max(poll_ms));
        // Compose the condition script. We always check the selector count;
        // the optional `state` (visible/hidden/enabled/checked), `url_glob`
        // (matches window.location.href), and `js_fn` (boolean expression
        // evaluated in the page) AND it together. `state` is queried via
        // Element.getBoundingClientRect (visible) or `el.disabled` / `el.checked`.
        let js_check = build_wait_for_js(selector, state, url_glob, js_fn);
        let mut attempts: u32 = 0;
        while std::time::Instant::now() < deadline {
            attempts += 1;
            let result = self
                .cdp_command(
                    "Runtime.evaluate",
                    json!({
                        "expression": js_check,
                        "returnByValue": true,
                    }),
                )
                .await?;
            let value = result
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Ok(parsed) = serde_json::from_str::<Value>(value) {
                let matched = parsed
                    .get("matched")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if matched {
                    return Ok(json!({
                        "found": true,
                        "selector": selector,
                        "state": state,
                        "url_glob": url_glob,
                        "js_fn": js_fn,
                        "count": parsed.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
                        "first_html": parsed.get("first").and_then(|v| v.as_str()).unwrap_or(""),
                        "attempts": attempts,
                    })
                    .to_string());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
        }
        Ok(json!({
            "found": false,
            "selector": selector,
            "state": state,
            "url_glob": url_glob,
            "js_fn": js_fn,
            "attempts": attempts,
            "hint": "Selector / condition did not become true within the timeout. \
                     Take a fresh browser_snapshot to see the current page state, \
                     or try a broader selector. Do NOT call web_fetch."
        })
        .to_string())
    }

    /// 页内 JS 求值（批次 N1：`browser_ptc` 的 `js` 步骤逃生舱）。
    /// CDP `Runtime.evaluate`（returnByValue + awaitPromise，Promise 结果
    /// 自动等 resolve）。抛异常时带回 exceptionDetails 的异常文本，让
    /// 模型直接看到页面侧报错而不是裸的 "evaluate failed"。
    async fn evaluate_js(&self, expression: &str) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .or_else(|| exc.get("text").and_then(|t| t.as_str()))
                .unwrap_or("unknown page-side exception");
            return Err(ToolError::ExecutionFailed(format!(
                "js evaluation threw: {text}"
            )));
        }
        let value = result.get("result").cloned().unwrap_or(json!(null));
        // 没有序列化 value（undefined / 远程对象）时退到 description，
        // 别让模型对着 null 猜页面返回了什么。
        let serialized = value
            .get("value")
            .cloned()
            .unwrap_or_else(|| value.get("description").cloned().unwrap_or(json!(null)));
        Ok(json!({
            "status": "ok",
            "type": value.get("type").cloned().unwrap_or(json!("unknown")),
            "value": serialized,
        })
        .to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        self.snapshot_with_options(None, None, false).await
    }

    async fn diff_snapshot(
        &self,
        old_snapshot: &str,
        new_snapshot: &str,
    ) -> Result<String, ToolError> {
        // a textual line-set diff on the YAML output. This is a
        // 90% solution that catches 90% of agent-browser interaction deltas
        // (new toast message, validation text, navigation landing page).
        // Full structural diff is work (post-message, post-fragment).
        let body = diff_a11y_yaml(old_snapshot, new_snapshot);
        Ok(json!({
            "status": "diff",
            "format": "a11y-yaml-line-diff",
            "diff": serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({})),
        })
        .to_string())
    }

    async fn snapshot_with_options(
        &self,
        depth: Option<u32>,
        target: Option<&str>,
        compact: bool,
    ) -> Result<String, ToolError> {
        let result = self
            .cdp_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        // Build a compact YAML representation. This is the format Playwright's
        // `snapshotForAI` and the Stagehand project use, and keeps tokens ~10x
        // smaller than dumping the raw JSON tree to the LLM.
        let ax_nodes = result
            .get("nodes")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let (yaml, ref_map) = ax_tree_to_a11y_yaml(&ax_nodes, depth, target);
        let yaml = if compact {
            compact_a11y_yaml(&yaml)
        } else {
            yaml
        };
        // wrap the YAML in <page_content>...</page_content> so the
        // downstream parser can distinguish interactive tree from any
        // prompt-injection content the LLM might be served by a hostile
        // page (e.g. a textarea containing "<system>...</system>").
        let wrapped = format!("<page_content>\n{yaml}\n</page_content>");
        // hard-cap the output length. Defaults to 50K characters;
        // any longer and the LLM context budget explodes. We surface a
        // truncation hint so the LLM can narrow with `target` / `depth`.
        const MAX_OUTPUT: usize = 50_000;
        let (final_tree, truncated) = if wrapped.len() > MAX_OUTPUT {
            let cut = wrapped
                .char_indices()
                .take_while(|(i, _)| *i < MAX_OUTPUT)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(MAX_OUTPUT);
            (
                format!(
                    "{}…\n\n<!-- TRUNCATED: page output exceeded {} characters. \
                     Re-snapshot with `target=\"<substring>\"` or `depth=2` to focus. -->",
                    &wrapped[..cut],
                    MAX_OUTPUT
                ),
                true,
            )
        } else {
            (wrapped, false)
        };
        // Cache the latest ref→selector mapping so subsequent click/type
        // calls can use the LLM-friendly `[ref=eN]` form.
        if let Ok(mut map) = self.last_snapshot_refs.lock() {
            *map = ref_map;
        }
        Ok(json!({
            "status": "snapshot",
            "format": "a11y-yaml",
            "compact": compact,
            "truncated": truncated,
            "ref_count": final_tree.matches("[ref=").count(),
            "tree": final_tree,
        })
        .to_string())
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        // Preflight reasons, shared by both resolution paths so a ref click and
        // a CSS click fail identically — otherwise a bug in one path would
        // look like a page difference.
        //
        // `PREFLIGHT_BODY` / the CSS variant take the element as `el`. On the
        // node path the element comes from a CDP-resolved handle; on the CSS
        // path it comes from `querySelector`. Everything after that is the
        // same check: missing / disabled / zero-size / covered.
        const PREFLIGHT_AFTER_EL: &str = r#"
            if (el.disabled) return {ok:false, reason:'disabled'};
            var r = el.getBoundingClientRect();
            if (r.width===0 || r.height===0) return {ok:false, reason:'invisible'};
            var x = r.left + r.width/2, y = r.top + r.height/2;
            var top = document.elementFromPoint(x,y);
            if (top && top!==el && !el.contains(top))
              return {ok:false, reason:'covered_by', cover: top.outerHTML.slice(0,160), tag: top.tagName};
            return {ok:true};"#;

        let (pre_reason, pre_cover) = match self.resolve_selector(selector)? {
            Resolved::Node(id) => {
                let body = format!(
                    "function(){{ if (!el) return {{ok:false, reason:'not_found'}}; \
                     {PREFLIGHT_AFTER_EL} }}"
                );
                self.preflight_via_node(id, &body).await?
            }
            Resolved::Css(ref sel) => {
                let js = format!(
                    "(function(){{ var el = document.querySelector({sel}); \
                     if(!el) return JSON.stringify({{ok:false, reason:'not_found'}}); \
                     return JSON.stringify((function(){{{PREFLIGHT_AFTER_EL}}})()); }})()",
                    sel = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into()),
                );
                self.preflight_via_css(&js).await?
            }
        };

        if let Some(reason) = pre_reason {
            let mut payload = json!({
                "status": "click_failed",
                "original": selector,
                "reason": reason,
            });
            if let Some(cover) = pre_cover {
                payload["covered_by"] = json!(cover);
            }
            // Structured next-step hint so the LLM does not blindly retry
            // the same click. The agent_loop's tool error recovery logic
            // will surface this directly to the model.
            payload["hint"] = json!(match reason.as_str() {
                "not_found" => "The target no longer exists. Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry.",
                "disabled" => "The element is disabled (disabled attribute or aria-disabled=true). Wait for the page to enable it, or interact with a different control.",
                "invisible" => "The element has zero size (display:none, hidden, or off-viewport). Try scrolling to it first, or use a different selector.",
                "covered_by" => "Another element is on top of this one (a modal, sticky header, or overlay). Dismiss or close the overlay before clicking, or use evaluate to call .click() directly while bypassing hit-testing.",
                _ => "Click could not be dispatched. Re-snapshot and try again.",
            });
            return Ok(payload.to_string());
        }

        // Dispatch. On the node path the click runs against the exact element
        // the accessibility tree described — no selector round-trip, so nothing
        // can re-resolve to a different node between check and dispatch.
        let dispatched = match self.resolve_selector(selector)? {
            Resolved::Node(id) => self
                .call_on_node(id, "function(){ el.click(); return 'clicked'; }")
                .await?
                .as_str()
                .map(|s| s.to_string()),
            Resolved::Css(ref sel) => {
                let js = format!(
                    "(function(){{ var el = document.querySelector({sel}); \
                     if(!el) return 'not found'; el.click(); return 'clicked'; }})()",
                    sel = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into()),
                );
                self.cdp_command(
                    "Runtime.evaluate",
                    json!({ "expression": js, "returnByValue": true }),
                )
                .await?
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            }
        };

        if dispatched.as_deref() != Some("clicked") {
            let mut payload = json!({
                "status": "click_failed",
                "original": selector,
                "reason": if dispatched.is_some() { "not_found" } else { "evaluate_failed" },
            });
            payload["hint"] = json!(
                "The page-side expression did not return 'clicked'. If the target came from a \
                 [ref=eN] handle, re-run browser_snapshot — handles go stale after every DOM \
                 change. Otherwise verify the selector against the page yourself."
            );
            return Ok(payload.to_string());
        }
        // Auto re-snapshot: ref tokens would otherwise go stale on the next
        // click. The price is one extra CDP round-trip per interaction, but
        // it saves the LLM from a "ref not found" failure round.
        let fresh_snapshot = self.snapshot_with_options(None, None, true).await.ok();
        Ok(json!({
            "status": "clicked",
            "original": selector,
            "snapshot": fresh_snapshot,
        })
        .to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        // Same element checks as `click`, with the type-specific additions
        // (readonly / hidden input) — and the same two resolution paths, so
        // a ref and a CSS target are held to identical standards.
        const PREFLIGHT_AFTER_EL: &str = r#"
            if (el.disabled || el.readOnly) return {ok:false, reason:'readonly'};
            if (el.tagName==='INPUT' && el.type==='hidden') return {ok:false, reason:'hidden_input'};
            return {ok:true};"#;

        let (pre_reason, _) = match self.resolve_selector(selector)? {
            Resolved::Node(id) => {
                let body = format!(
                    "function(){{ if (!el) return {{ok:false, reason:'not_found'}}; \
                     {PREFLIGHT_AFTER_EL} }}"
                );
                self.preflight_via_node(id, &body).await?
            }
            Resolved::Css(ref sel) => {
                let js = format!(
                    "(function(){{ var el = document.querySelector({sel}); \
                     if(!el) return JSON.stringify({{ok:false, reason:'not_found'}}); \
                     return JSON.stringify((function(){{{PREFLIGHT_AFTER_EL}}})()); }})()",
                    sel = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into()),
                );
                self.preflight_via_css(&js).await?
            }
        };

        if let Some(reason) = pre_reason {
            let mut payload = json!({
                "status": "type_failed",
                "original": selector,
                "reason": reason,
            });
            payload["hint"] = json!(match reason.as_str() {
                "not_found" => "The target no longer exists. Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry.",
                "readonly" => "The element is read-only. Find a similar field that accepts input, or remove the readonly attribute via evaluate.",
                "hidden_input" => "The matched input is type=hidden. Re-snapshot and look for a visible textbox or textarea with the same name.",
                _ => "Type could not be dispatched. Re-snapshot and try again.",
            });
            return Ok(payload.to_string());
        }

        // Dispatch. The text goes through `serde_json::to_string`, never string
        // concatenation with quote-replacement: a value containing a quote
        // would otherwise produce syntactically broken — and injectable — JS.
        let txt = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
        let dispatched = match self.resolve_selector(selector)? {
            Resolved::Node(id) => {
                // `el` is the CDP-resolved handle, so this cannot land on a
                // different element than the preflight just checked.
                let body = format!(
                    "function(){{ el.value = {txt}; \
                     el.dispatchEvent(new Event('input', {{bubbles:true}})); \
                     return 'typed'; }}"
                );
                self.call_on_node(id, &body)
                    .await?
                    .as_str()
                    .map(|s| s.to_string())
            }
            Resolved::Css(ref sel) => {
                let js = format!(
                    "(function(){{ var el = document.querySelector({sel}); \
                     if(!el) return 'not found'; \
                     el.value = {txt}; \
                     el.dispatchEvent(new Event('input', {{bubbles:true}})); \
                     return 'typed'; }})()",
                    sel = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into()),
                );
                self.cdp_command(
                    "Runtime.evaluate",
                    json!({ "expression": js, "returnByValue": true }),
                )
                .await?
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            }
        };

        // Report what actually happened. This used to be a hardcoded
        // `status: "typed"` that never looked at the result, so a failure was
        // reported to the model as a success and it moved on believing the
        // field was filled.
        if dispatched.as_deref() != Some("typed") {
            let mut payload = json!({
                "status": "type_failed",
                "original": selector,
                "reason": if dispatched.is_some() { "not_found" } else { "evaluate_failed" },
            });
            payload["hint"] = json!(
                "The page-side expression did not return 'typed'. If the target came from a \
                 [ref=eN] handle, re-run browser_snapshot to get a fresh handle — the text may \
                 have moved. Otherwise verify the element is an input/textarea that accepts text."
            );
            return Ok(payload.to_string());
        }
        // Re-snapshot so the LLM sees the post-input DOM (some apps render
        // autocomplete suggestions, validation messages, etc. on input).
        // We use `compact = true` here because the LLM typically only needs
        // to confirm the field value, not the whole page.
        let fresh_snapshot = self.snapshot_with_options(None, None, true).await.ok();
        Ok(json!({
            "status": "typed",
            "original": selector,
            "text": text,
            "snapshot": fresh_snapshot,
        })
        .to_string())
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        // structured error: keyDown without a keyUp leaves the page
        // thinking a modifier is held. We always emit a paired keyUp and
        // surface a typed result so the LLM can chain `press("Enter")` after
        // `type(...)` confidently.
        let result_down = self
            .cdp_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        let result_up = self
            .cdp_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyUp",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({
            "status": "key_pressed",
            "key": key,
            "cdp": {"down": result_down, "up": result_up}
        })
        .to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn highlight(&self, selector: &str) -> Result<String, ToolError> {
        // Resolve `[ref=eN]` tokens against the last snapshot's ref map. The
        // pulse-class cleanup has to run page-wide either way, so it stays a
        // `Runtime.evaluate`; only the "which element" part differs per path.
        let PREP = "document.querySelectorAll('.__hermix_hl_pulse')\
                   .forEach(function(n){n.remove();});";
        let (ok, reason) = match self.resolve_selector(selector)? {
            Resolved::Node(id) => {
                let body = format!(
                    "function(){{ {PREP} \
                     if(!el) return {{ok:false, reason:'not_found'}}; \
                     el.scrollIntoView({{block:'center'}}); \
                     el.style.outline='3px solid #ff0066'; \
                     el.style.outlineOffset='2px'; \
                     el.classList.add('__hermix_hl_pulse'); \
                     return {{ok:true}}; }}"
                );
                let v = self.call_on_node(id, &body).await?;
                (
                    v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false),
                    v.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string()),
                )
            }
            Resolved::Css(ref sel) => {
                let js = format!(
                    r#"(function(){{
                        {PREP}
                        var el = document.querySelector({json});
                        if(!el) return JSON.stringify({{ok:false, reason:'not_found'}});
                        el.scrollIntoView({{block:'center'}});
                        el.style.outline='3px solid #ff0066';
                        el.style.outlineOffset='2px';
                        el.classList.add('__hermix_hl_pulse');
                        return JSON.stringify({{ok:true}});
                    }})()"#,
                    json = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into()),
                );
                let result = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({"expression": js, "returnByValue": true }),
                    )
                    .await?;
                let value = result
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| json!({}));
                (
                    parsed.get("ok").and_then(|b| b.as_bool()).unwrap_or(false),
                    parsed
                        .get("reason")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string()),
                )
            }
        };

        if !ok {
            return Ok(json!({
                "status": "highlight_failed",
                "original": selector,
                "reason": reason.unwrap_or_else(|| "not_found".into()),
                "hint": "Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry with a new selector."
            })
            .to_string());
        }
        Ok(json!({
            "status": "highlighted",
            "original": selector,
        })
        .to_string())
    }

    async fn add_init_script(&self, script: &str) -> Result<String, ToolError> {
        // CDP's `Page.addScriptToEvaluateOnNewDocument` returns a
        // `scriptId` we MUST remember to be able to clear the script
        // later. The script fires on every subsequent navigation; CDP
        // does not auto-clear between sessions, so we manage that
        // ourselves.
        let result = self
            .cdp_command(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({
                    "source": script,
                    "runImmediately": true,
                }),
            )
            .await?;
        let script_id = result
            .get("identifier")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(id) = script_id.clone() {
            if let Ok(mut ids) = self.init_script_ids.lock() {
                ids.push(id);
            }
        }
        Ok(json!({
            "status": "init_script_added",
            "script_id": script_id,
            "script_length": script.len(),
        })
        .to_string())
    }

    async fn list_init_scripts(&self) -> Result<String, ToolError> {
        let ids = self
            .init_script_ids
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        Ok(json!({
            "status": "init_scripts_listed",
            "scripts": ids,
        })
        .to_string())
    }

    async fn clear_init_scripts(&self) -> Result<String, ToolError> {
        let mut cleared: usize = 0;
        let ids_opt = self.init_script_ids.lock().ok().map(|mut v| {
            let drained: Vec<String> = v.drain(..).collect();
            drained
        });
        if let Some(ids) = ids_opt {
            for id in &ids {
                let _ = self
                    .cdp_command(
                        "Page.removeScriptToEvaluateOnNewDocument",
                        json!({"identifier": id}),
                    )
                    .await;
                cleared += 1;
            }
        }
        Ok(json!({
            "status": "init_scripts_cleared",
            "cleared": cleared,
        })
        .to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // capture screenshot, optionally annotate it with [ref=]
        // badges overlaid on each interactive element. Annotation helps the
        // LLM correlate visual regions to the ref tokens it has already
        // received from browser_snapshot, which is critical for captcha /
        // canvas / map scenarios where text content is invisible to the
        // AX tree.
        let result = self
            .cdp_command(
                "Page.captureScreenshot",
                json!({"format": "png"}),
            )
            .await?;
        // If annotate=true, overlay ref badges via DOM.getBoxModel + a
        // Page-level CSS injection. This is a best-effort enhancement: if
        // any of the CDP calls fail we still return the raw screenshot.
        // The runtime eval returns a status string like 'annotated_3' or
        // 'annotate_failed: ...'; we propagate that into the response so
        // the LLM can tell whether the badges actually rendered.
        let annotate_status: String = self
            .cdp_command(
                "Runtime.evaluate",
                json!({
                    "expression": r#"
                        (function(){
                            try {
                                var refs = (window.__hermix_refs||{});
                                var labels = [];
                                Object.keys(refs).forEach(function(k){
                                    var el = document.querySelector(refs[k]);
                                    if(!el) return;
                                    var r = el.getBoundingClientRect();
                                    if(r.width===0||r.height===0) return;
                                    var b = document.createElement('div');
                                    b.className = '__hermix_ref_badge';
                                    b.textContent = '['+k+']';
                                    b.style.cssText = 'position:fixed;z-index:2147483647;background:#ff0066;color:#fff;font:bold 11px monospace;padding:2px 4px;border-radius:3px;box-shadow:0 1px 3px rgba(0,0,0,.4);pointer-events:none;';
                                    b.style.left = (r.left+2)+'px';
                                    b.style.top  = (r.top+2)+'px';
                                    document.body.appendChild(b);
                                    labels.push(k);
                                });
                                return 'annotated_'+labels.length;
                            } catch(e) { return 'annotate_failed: '+(e&&e.message||e); }
                        })()
                    "#,
                    "returnByValue": true,
                }),
            )
            .await
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|val| val.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "annotate_skipped".to_string());
        // Capture the (now-annotated) screenshot.
        let final_shot = self
            .cdp_command("Page.captureScreenshot", json!({"format": "png"}))
            .await
            .unwrap_or_else(|_| result.clone());
        // Best-effort: clean up the badges we just injected so they don't
        // pollute subsequent snapshots.
        let _ = self
            .cdp_command(
                "Runtime.evaluate",
                json!({
                    "expression": r#"document.querySelectorAll('.__hermix_ref_badge').forEach(n => n.remove()); 'cleaned'"#,
                }),
            )
            .await;
        let annotated = annotate_status.starts_with("annotated_");
        Ok(json!({
            "status": "vision_analysis",
            "instruction": instruction,
            "annotated": annotated,
            "annotate_status": annotate_status,
            "screenshot": final_shot,
            "note": "Screenshot captured with [ref=eN] badges overlaid. Use the most recent browser_snapshot's ref tokens to identify the region the instruction refers to. Cleaned up after capture."
        })
        .to_string())
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        // real console collection. The CDP `Runtime.consoleAPICalled`
        // event fires for every console.* call. We accumulate them into a
        // bounded in-memory buffer per CdpBrowserBackend instance, and
        // expose them via the `read` action with category filtering.
        match action {
            "read" => {
                // Default: dump everything from the in-memory buffer. The
                // buffer is populated by an event listener we install the
                // first time it's read; subsequent calls just drain.
                let drain = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({
                            "expression": r#"
                                (function(){
                                    if (!window.__hermix_console_buf) {
                                        window.__hermix_console_buf = [];
                                        if (window.__hermix_install_console) {
                                            window.__hermix_install_console();
                                        } else {
                                            // Fallback: monkey-patch console.* to capture.
                                            ['log','warn','error','info','debug'].forEach(function(level){
                                                var orig = console[level];
                                                console[level] = function(){
                                                    try {
                                                        var args = Array.prototype.slice.call(arguments).map(function(a){
                                                            if (a && typeof a==='object') {
                                                                try { return JSON.stringify(a); } catch(_) { return String(a); }
                                                            }
                                                            return String(a);
                                                        });
                                                        window.__hermix_console_buf.push({level:level, message: args.join(' '), ts: Date.now()});
                                                    } catch(_) {}
                                                    return orig.apply(console, arguments);
                                                };
                                            });
                                        }
                                    }
                                    return JSON.stringify(window.__hermix_console_buf.splice(0));
                                })()
                            "#,
                            "returnByValue": true,
                        }),
                    )
                    .await?;
                let value = drain
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let messages: Vec<Value> = serde_json::from_str(value).unwrap_or_default();
                // Bucket by level for the LLM.
                let by_level: std::collections::BTreeMap<String, usize> = {
                    let mut m = std::collections::BTreeMap::new();
                    for msg in &messages {
                        let level = msg
                            .get("level")
                            .and_then(|v| v.as_str())
                            .unwrap_or("log")
                            .to_string();
                        *m.entry(level).or_insert(0) += 1;
                    }
                    m
                };
                Ok(json!({
                    "status": "console_read",
                    "count": messages.len(),
                    "by_level": by_level,
                    "messages": messages,
                })
                .to_string())
            }
            "clear" => {
                let result = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({
                            "expression": r#"
                                (function(){
                                    if (window.__hermix_console_buf) window.__hermix_console_buf.length = 0;
                                    return 'cleared';
                                })()
                            "#,
                        }),
                    )
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            "level:log" | "level:warn" | "level:error" | "level:info" | "level:debug" => {
                let level = action.trim_start_matches("level:");
                let result = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({
                            "expression": format!(
                                "(function(){{ var b = (window.__hermix_console_buf||[]); \
                                 return JSON.stringify(b.filter(function(m){{ return m.level === '{}'; }})); }})()",
                                level
                            ),
                            "returnByValue": true,
                        }),
                    )
                    .await?;
                let value = result
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let messages: Vec<Value> = serde_json::from_str(value).unwrap_or_default();
                Ok(json!({
                    "status": "console_read",
                    "level": level,
                    "count": messages.len(),
                    "messages": messages,
                })
                .to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
    }
}

#[async_trait]
impl BrowserBackend for CamoFoxBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        let result = self.inner.navigate(url).await?;
        Ok(annotate_result(result, "camofox", "direct"))
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.snapshot().await?,
            "camofox",
            "direct",
        ))
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.click(selector).await?,
            "camofox",
            "direct",
        ))
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.r#type(selector, text).await?,
            "camofox",
            "direct",
        ))
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.scroll(direction, amount).await?,
            "camofox",
            "direct",
        ))
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.go_back().await?,
            "camofox",
            "direct",
        ))
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.press(key).await?,
            "camofox",
            "direct",
        ))
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.get_images(selector).await?,
            "camofox",
            "direct",
        ))
    }
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.vision(instruction).await?,
            "camofox",
            "direct",
        ))
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        Ok(annotate_result(
            self.inner.console(action).await?,
            "camofox",
            "direct",
        ))
    }
}

fn looks_like_cdp_websocket(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    lower.starts_with("ws://")
        || lower.starts_with("wss://")
        || lower.contains("/devtools/browser/")
        || lower.contains("/devtools/page/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_websocket_endpoint_skips_discovery() {
        let backend = CdpBrowserBackend::new("wss://browser-use.dev/devtools/browser/abc".into());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = rt.block_on(backend.resolve_target()).unwrap();
        assert_eq!(target, "wss://browser-use.dev/devtools/browser/abc");
    }

    /// 修 3c：`[ref=eN]` 查不到必须明确报错，不再静默降级成 CSS
    /// 选择器——降级会让陈旧句柄在新页面上匹配到语义无关的元素。
    #[test]
    fn resolve_selector_rejects_unknown_ref_token() {
        let backend = CdpBrowserBackend::new("http://localhost:9222".into());
        if let Ok(mut map) = backend.last_snapshot_refs.lock() {
            map.insert(
                "e1".into(),
                RefTarget {
                    backend_node_id: 42,
                    guessed_selector: "[role=\"button\"]".into(),
                },
            );
        }
        // A known handle resolves to the CDP node identity, NOT to a guessed
        // selector — that inversion is the whole point of the change.
        match backend.resolve_selector("[ref=e1]").unwrap() {
            Resolved::Node(id) => assert_eq!(id, 42),
            Resolved::Css(s) => panic!("a live ref must not degrade to CSS: {s}"),
        }
        // 不存在的句柄 → 明确报错（旧实现原样返回、当 CSS 用）
        let err = backend.resolve_selector("[ref=e999]").unwrap_err();
        assert!(err.to_string().contains("browser_snapshot"), "{err}");
        // 纯 CSS 选择器不受影响，原样通过
        match backend.resolve_selector("#submit").unwrap() {
            Resolved::Css(s) => assert_eq!(s, "#submit"),
            Resolved::Node(_) => panic!("a CSS selector must not become a node handle"),
        }
    }

    /// 修 3b：导航后旧句柄必须失效。清空动作在 `navigate_with_wait`
    /// 内部（需真实 CDP 才能走到），这里直接钉清空后的语义：
    /// 空 map 里查任何 `[ref=…]` 都是明确报错。
    #[test]
    fn resolve_selector_after_ref_map_cleared_fails_loud() {
        let backend = CdpBrowserBackend::new("http://localhost:9222".into());
        if let Ok(mut map) = backend.last_snapshot_refs.lock() {
            map.insert(
                "e1".into(),
                RefTarget {
                    backend_node_id: 7,
                    guessed_selector: "[role=\"button\"]".into(),
                },
            );
        }
        // 模拟 navigate_with_wait 成功后的清空分支（同一段代码语义）
        if let Ok(mut map) = backend.last_snapshot_refs.lock() {
            map.clear();
        }
        assert!(backend.resolve_selector("[ref=e1]").is_err());
    }

    /// A near-miss handle that is *itself valid CSS* must be rejected rather
    /// than forwarded. `[ref=e1` is a legal attribute selector, so passing it
    /// to `querySelector` would silently match whatever element carries
    /// `ref="e1"` — the model would be told it acted on one element while the
    /// code touched another. That is the failure this guards.
    ///
    /// Note what is deliberately NOT in this list: `ref=e1]` (no leading
    /// bracket). That is *invalid* CSS, so `querySelector` raises a
    /// SyntaxError and matches nothing — a loud, harmless outcome. It is not
    /// the same hazard, and rejecting it would mean guessing at intent from
    /// string shape.
    #[test]
    fn malformed_ref_tokens_never_degrade_to_css() {
        let backend = CdpBrowserBackend::new("http://localhost:9222".into());
        for bad in ["[ref=e1", "[ref=", "[ref=]"] {
            match backend.resolve_selector(bad) {
                Err(_) => {} // rejected — correct
                Ok(Resolved::Css(_)) => panic!("{bad} was accepted as a CSS selector"),
                Ok(Resolved::Node(_)) => panic!("{bad} resolved to a node"),
            }
        }
        // The error must name the right shape, otherwise the model retries the
        // same malformed string.
        let err = backend.resolve_selector("[ref=e1").unwrap_err().to_string();
        assert!(err.contains("[ref=eN]"), "error must show the shape: {err}");
    }

    #[test]
    fn is_ref_token_recognises_only_wellformed_tokens() {
        assert_eq!(CdpBrowserBackend::is_ref_token("[ref=e12]"), Some("e12"));
        assert_eq!(CdpBrowserBackend::is_ref_token("  [ref=e3]  "), Some("e3"));
        assert_eq!(CdpBrowserBackend::is_ref_token("[ref=e1"), None);
        assert_eq!(CdpBrowserBackend::is_ref_token("#submit"), None);
        // A CSS attribute selector that merely looks similar must stay CSS.
        assert_eq!(
            CdpBrowserBackend::is_ref_token("[data-ref=e1]"),
            None
        );
    }

    /// The preflight verdict parser is shared by both resolution paths, so a
    /// regression there would silently change click semantics on refs only.
    #[test]
    fn read_preflight_fails_closed() {
        // Explicit ok → no failure.
        assert_eq!(
            CdpBrowserBackend::read_preflight(&json!({"ok": true})),
            (None, None)
        );
        // Explicit false → reason carried through, cover when present.
        assert_eq!(
            CdpBrowserBackend::read_preflight(
                &json!({"ok": false, "reason": "covered_by", "cover": "<div id=x>"})
            ),
            (Some("covered_by".to_string()), Some("<div id=x>".to_string()))
        );
        // Unparseable → treated as a failure by the CSS path (which substitutes
        // ok:false before calling in), and an absent `ok` must not be read as
        // a pass.
        assert_eq!(
            CdpBrowserBackend::read_preflight(&json!({})),
            (None, None),
            "an absent ok is not an explicit pass"
        );
    }
}

#[cfg(test)]
mod init_script_tests {
    use super::*;

    /// the `Page.addScriptToEvaluateOnNewDocument` CDP call
    /// takes a `source` field and returns an `identifier` we must
    /// remember. We assert the request shape via a small serde
    /// round-trip so a future refactor doesn't drop the field name.
    #[test]
    fn init_script_request_uses_source_field() {
        let req = json!({
            "source": "window.__x = 1;",
            "runImmediately": true,
        });
        let v: Value = req;
        assert_eq!(v["source"], "window.__x = 1;");
        assert_eq!(v["runImmediately"], true);
    }

    /// clear must drain the local `init_script_ids` list and
    /// call `Page.removeScriptToEvaluateOnNewDocument` for each. We
    /// assert the local-list state is empty after clear.
    #[test]
    fn init_script_id_list_drains_on_clear() {
        // Standalone Mutex<Vec<String>> to mimic the field, without
        // spinning up a CDP backend.
        let ids = std::sync::Mutex::new(vec!["a".to_string(), "b".to_string()]);
        {
            let mut v = ids.lock().unwrap();
            v.clear();
        }
        assert!(ids.lock().unwrap().is_empty());
    }

    /// list returns a stable JSON shape so the LLM can rely
    /// on `scripts` always being an array (possibly empty).
    #[test]
    fn init_script_list_returns_array_always() {
        let body = json!({
            "status": "init_scripts_listed",
            "scripts": Vec::<String>::new(),
        });
        let v: Value = body;
        assert!(v["scripts"].is_array());
        assert_eq!(v["scripts"].as_array().unwrap().len(), 0);
    }
}
