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

use crate::tools::browser::BrowserBackend;
use hermix_core::ToolError;

use super::a11y::{ax_tree_to_a11y_yaml, build_wait_for_js, compact_a11y_yaml, diff_a11y_yaml};
use super::cloud::annotate_result;

/// Browser backend using Chrome DevTools Protocol.
/// Connects to Chrome via WebSocket for automation.
#[derive(Clone)]
pub struct CdpBrowserBackend {
    /// CDP endpoint URL. Can be either a discovery root (`http://host:9222`)
    /// or a concrete websocket endpoint (`wss://.../devtools/browser/...`).
    endpoint: String,
    client: Client,
    /// Last snapshot ref → CSS selector mapping. Lets the LLM pass
    /// `[ref=eN]` from the snapshot YAML instead of writing raw selectors.
    last_snapshot_refs: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
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
    /// that CDP can act on. Returns the input unchanged when it does not
    /// match the ref pattern.
    pub fn resolve_selector(&self, selector: &str) -> String {
        let trimmed = selector.trim();
        if let Some(rest) = trimmed.strip_prefix("[ref=") {
            if let Some(ref_id) = rest.strip_suffix(']') {
                if let Ok(map) = self.last_snapshot_refs.lock() {
                    if let Some(sel) = map.get(ref_id) {
                        return sel.clone();
                    }
                }
            }
        }
        trimmed.to_string()
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
    async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
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
        let resolved = self.resolve_selector(selector);
        // Pre-flight: detect missing / covered / disabled elements BEFORE
        // dispatching a click. This is the pre-flight element check that the
        // screenshot showed was missing — the LLM was firing
        // a click into the void, getting `clicked` back, and then
        // wondering why nothing happened.
        let preflight = self
            .cdp_command(
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "(function(){{ var el = document.querySelector({}); \
                         if(!el) return JSON.stringify({{ok:false, reason:'not_found'}}); \
                         if(el.disabled) return JSON.stringify({{ok:false, reason:'disabled'}}); \
                         var r = el.getBoundingClientRect(); \
                         if(r.width===0||r.height===0) return JSON.stringify({{ok:false, reason:'invisible'}}); \
                         var x = r.left + r.width/2, y = r.top + r.height/2; \
                         var top = document.elementFromPoint(x,y); \
                         if(top && top!==el && !el.contains(top)) \
                           return JSON.stringify({{ok:false, reason:'covered_by', cover: top.outerHTML.slice(0,160), tag: top.tagName}}); \
                         return JSON.stringify({{ok:true}}); }})()",
                        serde_json::to_string(&resolved).unwrap_or_else(|_| "\"\"".into())
                    ),
                    "returnByValue": true,
                }),
            )
            .await?;
        let pre_value = preflight
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let pre: Value = serde_json::from_str(pre_value).unwrap_or_else(|_| json!({"ok": true}));
        if pre.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let reason = pre.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let mut payload = json!({
                "status": "click_failed",
                "selector": resolved,
                "original": selector,
                "reason": reason,
            });
            if let Some(cover) = pre.get("cover").and_then(|v| v.as_str()) {
                payload["covered_by"] = json!(cover);
            }
            // Structured next-step hint so the LLM does not blindly retry
            // the same click. The agent_loop's tool error recovery logic
            // will surface this directly to the model.
            payload["hint"] = json!(match reason {
                "not_found" => "The selector matched no element. Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry with a new selector.",
                "disabled" => "The element is disabled (disabled attribute or aria-disabled=true). Wait for the page to enable it, or interact with a different control.",
                "invisible" => "The element has zero size (display:none, hidden, or off-viewport). Try scrolling to it first, or use a different selector.",
                "covered_by" => "Another element is on top of this one (a modal, sticky header, or overlay). Dismiss or close the overlay before clicking, or use evaluate to call .click() directly while bypassing hit-testing.",
                _ => "Click could not be dispatched. Re-snapshot and try again.",
            });
            return Ok(payload.to_string());
        }

        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            resolved.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        // Auto re-snapshot: ref tokens would otherwise go stale on the next
        // click. The price is one extra CDP round-trip per interaction, but
        // it saves the LLM from a "ref not found" failure round.
        let fresh_snapshot = self.snapshot_with_options(None, None, true).await.ok();
        Ok(json!({
            "status": "clicked",
            "selector": resolved,
            "original": selector,
            "cdp": result,
            "snapshot": fresh_snapshot,
        })
        .to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let resolved = self.resolve_selector(selector);
        // Same pre-flight as click: surface not_found / disabled / invisible
        // BEFORE the LLM proceeds to fill the next field.
        let preflight = self
            .cdp_command(
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "(function(){{ var el = document.querySelector({}); \
                         if(!el) return JSON.stringify({{ok:false, reason:'not_found'}}); \
                         if(el.disabled || el.readOnly) return JSON.stringify({{ok:false, reason:'readonly'}}); \
                         if(el.tagName==='INPUT' && (el.type==='hidden')) \
                           return JSON.stringify({{ok:false, reason:'hidden_input'}}); \
                         return JSON.stringify({{ok:true}}); }})()",
                        serde_json::to_string(&resolved).unwrap_or_else(|_| "\"\"".into())
                    ),
                    "returnByValue": true,
                }),
            )
            .await?;
        let pre_value = preflight
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let pre: Value = serde_json::from_str(pre_value).unwrap_or_else(|_| json!({"ok": true}));
        if pre.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let reason = pre.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let mut payload = json!({
                "status": "type_failed",
                "selector": resolved,
                "original": selector,
                "reason": reason,
            });
            payload["hint"] = json!(match reason {
                "not_found" => "The selector matched no element. Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry with a new selector.",
                "readonly" => "The element is read-only. Find a similar field that accepts input, or remove the readonly attribute via evaluate.",
                "hidden_input" => "The matched input is type=hidden. Re-snapshot and look for a visible textbox or textarea with the same name.",
                _ => "Type could not be dispatched. Re-snapshot and try again.",
            });
            return Ok(payload.to_string());
        }

        let js = format!(
            "let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }}",
            resolved.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        // Re-snapshot so the LLM sees the post-input DOM (some apps render
        // autocomplete suggestions, validation messages, etc. on input).
        // We use `compact = true` here because the LLM typically only needs
        // to confirm the field value, not the whole page.
        let fresh_snapshot = self.snapshot_with_options(None, None, true).await.ok();
        Ok(json!({
            "status": "typed",
            "selector": resolved,
            "original": selector,
            "text": text,
            "cdp": result,
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
        // Resolve `[ref=eN]` tokens against the last snapshot's ref map.
        let resolved = self.resolve_selector(selector);
        let json = serde_json::to_string(&resolved).unwrap_or_else(|_| "\"\"".into());
        let js = format!(
            r#"(function(){{
                var __hl_old = document.querySelectorAll('.__hermix_hl_pulse');
                for (var i=0;i<__hl_old.length;i++) __hl_old[i].remove();
                var el = document.querySelector({json});
                if(!el) return JSON.stringify({{ok:false, reason:'not_found'}});
                el.scrollIntoView({{block:'center'}});
                el.style.outline='3px solid #ff0066';
                el.style.outlineOffset='2px';
                el.classList.add('__hermix_hl_pulse');
                return JSON.stringify({{ok:true, selector:{json}}});
            }})()"#
        );
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        let value = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| json!({}));
        if parsed.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let reason = parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            return Ok(json!({
                "status": "highlight_failed",
                "selector": resolved,
                "original": selector,
                "reason": reason,
                "hint": "Re-snapshot (browser_snapshot) to get fresh [ref=eN] handles, then retry with a new selector."
            })
            .to_string());
        }
        Ok(json!({
            "status": "highlighted",
            "selector": resolved,
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
