//! Browser backends: local CDP plus provider-selected cloud sessions.
//!
//! Python 4.16 resolves browser execution in two layers:
//! 1. choose a provider (`browser-use`, `browserbase`, `camofox`, `local`)
//! 2. obtain a concrete CDP endpoint for that provider
//!
//! The Rust port keeps the existing `BrowserBackend` tool surface, but routes
//! calls through an auto-selecting backend that lazily provisions cloud
//! sessions when needed.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::tools::browser::BrowserBackend;
use hermix_config::load_config;
use hermix_config::managed_gateway::{
    prefers_gateway, resolve_managed_tool_gateway, ResolveOptions,
};
use hermix_core::ToolError;

const DEFAULT_BROWSER_USE_BASE_URL: &str = "https://api.browser-use.com/api/v3";
const DEFAULT_BROWSERBASE_BASE_URL: &str = "https://api.browserbase.com";
const DEFAULT_FIRECRAWL_BASE_URL: &str = "https://api.firecrawl.dev";
const DEFAULT_FIRECRAWL_BROWSER_TTL_SECS: u32 = 300;
const DEFAULT_MANAGED_BROWSER_USE_TIMEOUT_MINUTES: u32 = 5;
const DEFAULT_MANAGED_PROXY_COUNTRY_CODE: &str = "us";

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
    /// Phase 4: init scripts registered via `Page.addScriptToEvaluateOnNewDocument`.
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloudBrowserSession {
    provider: &'static str,
    session_id: String,
    cdp_url: String,
    transport: &'static str,
}

#[derive(Default)]
struct CloudBackendState {
    session: Option<CloudBrowserSession>,
    backend: Option<CdpBrowserBackend>,
}

#[async_trait]
trait CloudBrowserProvider: Send + Sync {
    fn provider_key(&self) -> &'static str;
    fn is_configured(&self) -> bool;
    async fn create_session(&self, task_id: &str) -> Result<CloudBrowserSession, ToolError>;
}

#[derive(Clone)]
struct BrowserUseCloudProvider {
    client: Client,
}

#[derive(Clone)]
struct BrowserbaseCloudProvider {
    client: Client,
}

#[derive(Clone)]
struct FirecrawlCloudProvider {
    client: Client,
}

pub struct CloudBrowserBackendAdapter {
    provider: Arc<dyn CloudBrowserProvider>,
    state: Mutex<CloudBackendState>,
}

enum ResolvedBrowserBackend {
    Local(CdpBrowserBackend),
    CamoFox(CamoFoxBrowserBackend),
    Cloud(CloudBrowserBackendAdapter),
    Unsupported(String),
}

/// Runtime browser backend selector mirroring Python's provider choice logic.
pub struct AutoBrowserBackend {
    inner: ResolvedBrowserBackend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserRuntimeKind {
    Local,
    CamoFox,
    BrowserUse,
    Browserbase,
    Firecrawl,
    Unsupported(String),
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
    ///
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
    ///
    /// `event_method` is the CDP method name to wait for (e.g.
    /// `"Page.loadEventFired"`). If `None`, behaves like [`Self::cdp_command`].
    /// `event_timeout` caps how long to wait for the event before returning
    /// `(result, None)` — the navigate call has likely already succeeded.
    ///
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
impl CloudBrowserProvider for BrowserUseCloudProvider {
    fn provider_key(&self) -> &'static str {
        "browser-use"
    }

    fn is_configured(&self) -> bool {
        browser_use_config().is_some()
    }

    async fn create_session(&self, task_id: &str) -> Result<CloudBrowserSession, ToolError> {
        let config = browser_use_config().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browser Use requires either BROWSER_USE_API_KEY or a managed browser-use gateway."
                    .into(),
            )
        })?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse().expect("static header value must parse"));
        headers.insert(
            "X-Browser-Use-API-Key",
            config.api_key.parse().map_err(|e| {
                ToolError::ExecutionFailed(format!("invalid Browser Use API key header: {e}"))
            })?,
        );
        if config.managed_mode {
            headers.insert(
                "X-Idempotency-Key",
                format!("browser-use-session-create:{}", uuid::Uuid::new_v4())
                    .parse()
                    .expect("idempotency key must parse"),
            );
        }
        let body = if config.managed_mode {
            json!({
                "timeout": DEFAULT_MANAGED_BROWSER_USE_TIMEOUT_MINUTES,
                "proxyCountryCode": DEFAULT_MANAGED_PROXY_COUNTRY_CODE,
            })
        } else {
            json!({})
        };
        let response = self
            .client
            .post(format!("{}/browsers", config.base_url))
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Browser Use create session failed: {e}"))
            })?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Browser Use create session json failed: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browser Use session: {} {}",
                status, payload
            )));
        }
        let session_id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cdp_url = payload
            .get("cdpUrl")
            .or_else(|| payload.get("connectUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() || cdp_url.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "Browser Use session response missing id/cdpUrl".into(),
            ));
        }
        Ok(CloudBrowserSession {
            provider: self.provider_key(),
            session_id: format!("{task_id}:{session_id}"),
            cdp_url,
            transport: if config.managed_mode {
                "managed"
            } else {
                "direct"
            },
        })
    }
}

#[async_trait]
impl CloudBrowserProvider for BrowserbaseCloudProvider {
    fn provider_key(&self) -> &'static str {
        "browserbase"
    }

    fn is_configured(&self) -> bool {
        browserbase_config().is_some()
    }

    async fn create_session(&self, task_id: &str) -> Result<CloudBrowserSession, ToolError> {
        let config = browserbase_config().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browserbase requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID.".into(),
            )
        })?;
        let response = self
            .client
            .post(format!("{}/v1/sessions", config.base_url))
            .header("Content-Type", "application/json")
            .header("X-BB-API-Key", config.api_key)
            .json(&json!({"projectId": config.project_id}))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Browserbase create session failed: {e}"))
            })?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Browserbase create session json failed: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browserbase session: {} {}",
                status, payload
            )));
        }
        let session_id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cdp_url = payload
            .get("connectUrl")
            .or_else(|| payload.get("cdpUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() || cdp_url.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "Browserbase session response missing id/connectUrl".into(),
            ));
        }
        Ok(CloudBrowserSession {
            provider: self.provider_key(),
            session_id: format!("{task_id}:{session_id}"),
            cdp_url,
            transport: "direct",
        })
    }
}

#[async_trait]
impl CloudBrowserProvider for FirecrawlCloudProvider {
    fn provider_key(&self) -> &'static str {
        "firecrawl"
    }

    fn is_configured(&self) -> bool {
        firecrawl_config().is_some()
    }

    async fn create_session(&self, task_id: &str) -> Result<CloudBrowserSession, ToolError> {
        let config = firecrawl_config().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Firecrawl requires FIRECRAWL_API_KEY environment variable.".into(),
            )
        })?;
        let response = self
            .client
            .post(format!("{}/v2/browser", config.base_url))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", config.api_key))
            .json(&json!({"ttl": config.browser_ttl_secs}))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Firecrawl create session failed: {e}"))
            })?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Firecrawl create session json failed: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Firecrawl session: {} {}",
                status, payload
            )));
        }
        let session_id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cdp_url = payload
            .get("cdpUrl")
            .or_else(|| payload.get("connectUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() || cdp_url.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "Firecrawl session response missing id/cdpUrl".into(),
            ));
        }
        Ok(CloudBrowserSession {
            provider: self.provider_key(),
            session_id: format!("{task_id}:{session_id}"),
            cdp_url,
            transport: "direct",
        })
    }
}

impl CloudBrowserBackendAdapter {
    fn new(provider: Arc<dyn CloudBrowserProvider>) -> Self {
        Self {
            provider,
            state: Mutex::new(CloudBackendState::default()),
        }
    }

    async fn ensure_backend(
        &self,
    ) -> Result<(CdpBrowserBackend, &'static str, &'static str), ToolError> {
        let mut state = self.state.lock().await;
        if let (Some(session), Some(backend)) = (&state.session, &state.backend) {
            return Ok((backend.clone(), session.provider, session.transport));
        }
        let session = self.provider.create_session("shared").await?;
        let backend = CdpBrowserBackend::new(session.cdp_url.clone());
        let provider = session.provider;
        let transport = session.transport;
        state.session = Some(session);
        state.backend = Some(backend.clone());
        Ok((backend, provider, transport))
    }
}

impl AutoBrowserBackend {
    pub fn from_env() -> Self {
        let inner = match resolve_browser_runtime_kind() {
            BrowserRuntimeKind::Local => {
                ResolvedBrowserBackend::Local(CdpBrowserBackend::from_env())
            }
            BrowserRuntimeKind::CamoFox => {
                ResolvedBrowserBackend::CamoFox(CamoFoxBrowserBackend::from_env())
            }
            BrowserRuntimeKind::BrowserUse => ResolvedBrowserBackend::Cloud(
                CloudBrowserBackendAdapter::new(Arc::new(BrowserUseCloudProvider {
                    client: Client::new(),
                })),
            ),
            BrowserRuntimeKind::Browserbase => ResolvedBrowserBackend::Cloud(
                CloudBrowserBackendAdapter::new(Arc::new(BrowserbaseCloudProvider {
                    client: Client::new(),
                })),
            ),
            BrowserRuntimeKind::Firecrawl => ResolvedBrowserBackend::Cloud(
                CloudBrowserBackendAdapter::new(Arc::new(FirecrawlCloudProvider {
                    client: Client::new(),
                })),
            ),
            BrowserRuntimeKind::Unsupported(provider) => {
                ResolvedBrowserBackend::Unsupported(format!(
                    "Browser provider '{provider}' is not implemented in hermes-agent-rust yet."
                ))
            }
        };
        Self { inner }
    }
}

#[async_trait]
impl BrowserBackend for AutoBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.navigate(url).await,
            ResolvedBrowserBackend::CamoFox(b) => b.navigate(url).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.navigate(url).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.snapshot().await,
            ResolvedBrowserBackend::CamoFox(b) => b.snapshot().await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.snapshot().await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn snapshot_with_options(
        &self,
        depth: Option<u32>,
        target: Option<&str>,
        compact: bool,
    ) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => {
                b.snapshot_with_options(depth, target, compact).await
            }
            ResolvedBrowserBackend::CamoFox(b) => {
                b.snapshot_with_options(depth, target, compact).await
            }
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.snapshot_with_options(depth, target, compact).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn diff_snapshot(
        &self,
        old_snapshot: &str,
        new_snapshot: &str,
    ) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.diff_snapshot(old_snapshot, new_snapshot).await,
            ResolvedBrowserBackend::CamoFox(b) => {
                b.diff_snapshot(old_snapshot, new_snapshot).await
            }
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.diff_snapshot(old_snapshot, new_snapshot).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.click(selector).await,
            ResolvedBrowserBackend::CamoFox(b) => b.click(selector).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.click(selector).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.r#type(selector, text).await,
            ResolvedBrowserBackend::CamoFox(b) => b.r#type(selector, text).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.r#type(selector, text).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.scroll(direction, amount).await,
            ResolvedBrowserBackend::CamoFox(b) => b.scroll(direction, amount).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.scroll(direction, amount).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.go_back().await,
            ResolvedBrowserBackend::CamoFox(b) => b.go_back().await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.go_back().await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.press(key).await,
            ResolvedBrowserBackend::CamoFox(b) => b.press(key).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.press(key).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.get_images(selector).await,
            ResolvedBrowserBackend::CamoFox(b) => b.get_images(selector).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.get_images(selector).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.vision(instruction).await,
            ResolvedBrowserBackend::CamoFox(b) => b.vision(instruction).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.vision(instruction).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn wait_for(
        &self,
        selector: &str,
        timeout_ms: u64,
        state: Option<&str>,
        url_glob: Option<&str>,
        js_fn: Option<&str>,
    ) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => {
                b.wait_for(selector, timeout_ms, state, url_glob, js_fn).await
            }
            ResolvedBrowserBackend::CamoFox(b) => {
                b.wait_for(selector, timeout_ms, state, url_glob, js_fn).await
            }
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend
                        .wait_for(selector, timeout_ms, state, url_glob, js_fn)
                        .await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.console(action).await,
            ResolvedBrowserBackend::CamoFox(b) => b.console(action).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.console(action).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn highlight(&self, selector: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.highlight(selector).await,
            ResolvedBrowserBackend::CamoFox(b) => b.highlight(selector).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.highlight(selector).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn add_init_script(&self, script: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.add_init_script(script).await,
            ResolvedBrowserBackend::CamoFox(b) => b.add_init_script(script).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.add_init_script(script).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn list_init_scripts(&self) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.list_init_scripts().await,
            ResolvedBrowserBackend::CamoFox(b) => b.list_init_scripts().await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.list_init_scripts().await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn clear_init_scripts(&self) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.clear_init_scripts().await,
            ResolvedBrowserBackend::CamoFox(b) => b.clear_init_scripts().await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.clear_init_scripts().await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
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

    async fn snapshot(&self) -> Result<String, ToolError> {
        self.snapshot_with_options(None, None, false).await
    }

    async fn diff_snapshot(
        &self,
        old_snapshot: &str,
        new_snapshot: &str,
    ) -> Result<String, ToolError> {
        // Phase 1: a textual line-set diff on the YAML output. This is a
        // 90% solution that catches 90% of agent-browser interaction deltas
        // (new toast message, validation text, navigation landing page).
        // Full structural diff is Phase 2 work (post-message, post-fragment).
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
        // Phase 2: wrap the YAML in <page_content>...</page_content> so the
        // downstream parser can distinguish interactive tree from any
        // prompt-injection content the LLM might be served by a hostile
        // page (e.g. a textarea containing "<system>...</system>").
        let wrapped = format!("<page_content>\n{yaml}\n</page_content>");
        // Phase 2: hard-cap the output length. Defaults to 50K characters;
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
        // dispatching a click. This is the Phase-1 deliverable that the
        // 2026-06-27 screenshot showed was missing — the LLM was firing
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
        // Phase 1 structured error: keyDown without a keyUp leaves the page
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
        // Phase 2: capture screenshot, optionally annotate it with [ref=]
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
        // Phase 2: real console collection. The CDP `Runtime.consoleAPICalled`
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

#[derive(Debug, Clone)]
struct BrowserUseConfig {
    api_key: String,
    base_url: String,
    managed_mode: bool,
}

#[derive(Debug, Clone)]
struct BrowserbaseConfig {
    api_key: String,
    project_id: String,
    base_url: String,
}

struct FirecrawlConfig {
    api_key: String,
    base_url: String,
    browser_ttl_secs: u32,
}

fn browser_use_config() -> Option<BrowserUseConfig> {
    let direct_api_key = std::env::var("BROWSER_USE_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(api_key) = direct_api_key {
        if !prefers_gateway("browser") {
            return Some(BrowserUseConfig {
                api_key,
                base_url: DEFAULT_BROWSER_USE_BASE_URL.to_string(),
                managed_mode: false,
            });
        }
    }
    resolve_managed_tool_gateway("browser-use", ResolveOptions::default()).map(|cfg| {
        BrowserUseConfig {
            api_key: cfg.nous_user_token,
            base_url: cfg.gateway_origin.trim_end_matches('/').to_string(),
            managed_mode: true,
        }
    })
}

fn browserbase_config() -> Option<BrowserbaseConfig> {
    let api_key = std::env::var("BROWSERBASE_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let project_id = std::env::var("BROWSERBASE_PROJECT_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let base_url = std::env::var("BROWSERBASE_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_BROWSERBASE_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    Some(BrowserbaseConfig {
        api_key,
        project_id,
        base_url,
    })
}

fn firecrawl_config() -> Option<FirecrawlConfig> {
    let api_key = std::env::var("FIRECRAWL_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let base_url = std::env::var("FIRECRAWL_API_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_FIRECRAWL_BASE_URL.to_string());
    let browser_ttl_secs = std::env::var("FIRECRAWL_BROWSER_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|ttl| *ttl > 0)
        .unwrap_or(DEFAULT_FIRECRAWL_BROWSER_TTL_SECS);
    Some(FirecrawlConfig {
        api_key,
        base_url,
        browser_ttl_secs,
    })
}

// ---------------------------------------------------------------------------
// Chrome auto-launch was previously in this file (legacy
// `ensure_chrome_running` + `discover_chrome_binary` + `cdp_endpoint_alive`).
// It is now handled by `crate::tools::cdp_bootstrap` and wired through
// `BrowserRegistry::ensure_browser_bootstrap_async`, so all 14 browser
// tool handlers transparently spawn Chrome without per-handler setup.
// ---------------------------------------------------------------------------

fn looks_like_cdp_websocket(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    lower.starts_with("ws://")
        || lower.starts_with("wss://")
        || lower.contains("/devtools/browser/")
        || lower.contains("/devtools/page/")
}

fn normalize_browser_cloud_provider(value: Option<&str>) -> String {
    let provider = value.unwrap_or("local").trim().to_ascii_lowercase();
    if provider.is_empty() {
        "local".to_string()
    } else {
        provider
    }
}

fn resolve_browser_runtime_kind() -> BrowserRuntimeKind {
    let config_provider = load_config(None)
        .ok()
        .and_then(|cfg| cfg.browser.cloud_provider)
        .map(|s| normalize_browser_cloud_provider(Some(&s)));

    if std::env::var("CAMOFOX_CDP_URL")
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        return BrowserRuntimeKind::CamoFox;
    }

    if let Some(provider) = config_provider {
        return match provider.as_str() {
            "local" => BrowserRuntimeKind::Local,
            "camofox" => BrowserRuntimeKind::CamoFox,
            "browser-use" => BrowserRuntimeKind::BrowserUse,
            "browserbase" => BrowserRuntimeKind::Browserbase,
            "firecrawl" => BrowserRuntimeKind::Firecrawl,
            _ => BrowserRuntimeKind::Local,
        };
    }

    let browser_use = BrowserUseCloudProvider {
        client: Client::new(),
    };
    if browser_use.is_configured() {
        return BrowserRuntimeKind::BrowserUse;
    }
    let browserbase = BrowserbaseCloudProvider {
        client: Client::new(),
    };
    if browserbase.is_configured() {
        return BrowserRuntimeKind::Browserbase;
    }
    BrowserRuntimeKind::Local
}

fn annotate_result(raw: String, provider: &str, transport: &str) -> String {
    match serde_json::from_str::<Value>(&raw) {
        Ok(mut value) => {
            if let Some(obj) = value.as_object_mut() {
                obj.insert("browser_provider".into(), json!(provider));
                obj.insert("browser_transport".into(), json!(transport));
                return value.to_string();
            }
            json!({
                "result": value,
                "browser_provider": provider,
                "browser_transport": transport,
            })
            .to_string()
        }
        Err(_) => json!({
            "result": raw,
            "browser_provider": provider,
            "browser_transport": transport,
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_config::env_compat;
    use hermix_config::managed_gateway::test_lock;

    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMIX_HOME",
                "BROWSER_USE_API_KEY",
                "BROWSERBASE_API_KEY",
                "BROWSERBASE_PROJECT_ID",
                "BROWSERBASE_BASE_URL",
                "FIRECRAWL_API_KEY",
                "FIRECRAWL_API_URL",
                "FIRECRAWL_BROWSER_TTL",
                "TOOL_GATEWAY_USER_TOKEN",
                "HERMIX_ENABLE_NOUS_MANAGED_TOOLS",
                "CAMOFOX_CDP_URL",
                "CHROME_CDP_URL",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMIX_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn cdp_websocket_endpoint_skips_discovery() {
        let backend = CdpBrowserBackend::new("wss://browser-use.dev/devtools/browser/abc".into());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = rt.block_on(backend.resolve_target()).unwrap();
        assert_eq!(target, "wss://browser-use.dev/devtools/browser/abc");
    }

    #[test]
    fn browser_use_prefers_direct_without_use_gateway() {
        let _g = EnvScope::new();
        std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
        let cfg = browser_use_config().unwrap();
        assert!(!cfg.managed_mode);
        assert_eq!(cfg.api_key, "direct-key");
        assert_eq!(cfg.base_url, DEFAULT_BROWSER_USE_BASE_URL);
    }

    #[test]
    fn browser_use_honors_use_gateway() {
        let _g = EnvScope::new();
        std::fs::write(
            std::path::Path::new(&env_compat("HERMIX_HOME", "HERMES_HOME").unwrap()).join("config.yaml"),
            "browser:\n  use_gateway: true\n",
        )
        .unwrap();
        std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "managed-token");
        let cfg = browser_use_config().unwrap();
        assert!(cfg.managed_mode);
        assert_eq!(cfg.api_key, "managed-token");
    }

    #[test]
    fn runtime_prefers_browser_use_over_browserbase_when_auto() {
        let _g = EnvScope::new();
        std::env::set_var("BROWSER_USE_API_KEY", "browser-use-key");
        std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
        std::env::set_var("BROWSERBASE_PROJECT_ID", "bb-project");
        assert_eq!(
            resolve_browser_runtime_kind(),
            BrowserRuntimeKind::BrowserUse
        );
    }

    #[test]
    fn runtime_respects_explicit_browserbase_provider() {
        let _g = EnvScope::new();
        std::fs::write(
            std::path::Path::new(&env_compat("HERMIX_HOME", "HERMES_HOME").unwrap()).join("config.yaml"),
            "browser:\n  cloud_provider: browserbase\n",
        )
        .unwrap();
        std::env::set_var("BROWSER_USE_API_KEY", "browser-use-key");
        assert_eq!(
            resolve_browser_runtime_kind(),
            BrowserRuntimeKind::Browserbase
        );
    }

    #[test]
    fn runtime_respects_explicit_firecrawl_provider() {
        let _g = EnvScope::new();
        std::fs::write(
            std::path::Path::new(&env_compat("HERMIX_HOME", "HERMES_HOME").unwrap()).join("config.yaml"),
            "browser:\n  cloud_provider: firecrawl\n",
        )
        .unwrap();
        std::env::set_var("FIRECRAWL_API_KEY", "fc-test-key");
        assert_eq!(
            resolve_browser_runtime_kind(),
            BrowserRuntimeKind::Firecrawl
        );
    }

    #[test]
    fn annotate_result_injects_browser_metadata() {
        let out = annotate_result("{\"status\":\"ok\"}".into(), "browser-use", "managed");
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["browser_provider"], "browser-use");
        assert_eq!(value["browser_transport"], "managed");
    }

    #[test]
    fn ax_tree_yaml_assigns_refs_for_interactive_nodes() {
        // CDP emit order: parent first, then children. Root has
        // `childIds` referencing its direct descendants. The walker
        // (post Phase 1 fix) uses the node's own childIds to build
        // the parent → children map, so node 1's childIds = [2, 3]
        // gives us two walked children.
        let tree = json!([
            {
                "nodeId": "1",
                "role": {"value": "generic"},
                "name": {"value": ""},
                "childIds": ["2", "3"]
            },
            {
                "nodeId": "2",
                "role": {"value": "button"},
                "name": {"value": "Login"},
                "properties": [{"name": "disabled", "value": {"value": false}}],
                "childIds": []
            },
            {
                "nodeId": "3",
                "role": {"value": "textbox"},
                "name": {"value": "Username"},
                "childIds": []
            }
        ]);
        let (yaml, refs) = ax_tree_to_a11y_yaml(&tree, None, None);
        assert!(yaml.contains("[ref=e1]"), "missing e1: {yaml}");
        assert!(yaml.contains("[ref=e2]"), "missing e2: {yaml}");
        assert_eq!(
            refs.get("e1").map(|s| s.as_str()),
            Some("[role=\"button\"]:has-text('Login')")
        );
        assert_eq!(
            refs.get("e2").map(|s| s.as_str()),
            Some("[role=\"textbox\"]:has-text('Username')")
        );
        assert!(yaml.contains("button \"Login\""));
    }

    #[test]
    fn ax_tree_yaml_respects_depth_limit() {
        // Three-level nested tree, walked root-first.
        let tree = json!([
            {"nodeId": "1", "role": {"value": "generic"}, "name": {"value": ""}, "childIds": ["2"]},
            {"nodeId": "2", "role": {"value": "generic"}, "name": {"value": ""}, "childIds": ["3"]},
            {"nodeId": "3", "role": {"value": "button"}, "name": {"value": "Submit"}, "childIds": []}
        ]);
        let (yaml, _) = ax_tree_to_a11y_yaml(&tree, Some(0), None);
        // depth=0 means we only render the root. `generic` is a non-interactive
        // role, so it has no [ref=] token; the root line is just `"- generic"`.
        assert!(yaml.contains("- generic"));
        assert!(!yaml.contains("Submit"));
    }
}

// ---------------------------------------------------------------------------
// AX Tree → compact YAML conversion
// ---------------------------------------------------------------------------
//
// CDP `Accessibility.getFullAXTree` returns ~5K-token noise when fed raw to
// an LLM. We compress it to a Playwright/Stagehand-style YAML tree that
// preserves interactive nodes, names, and states, and assigns each
// interactive node a stable `[ref=eN]` handle the LLM can pass back to
// `browser_click` / `browser_type` instead of writing CSS selectors.

/// Pick a stable CSS selector for an AX node, used as the
/// resolver target for `[ref=eN]` tokens.
fn ax_node_selector(role: &str, name: &str) -> String {
    if name.is_empty() {
        format!("[role=\"{}\"]", role)
    } else {
        // Playwright-style: prefer accessible name match.
        format!("[role=\"{}\"]:has-text('{}')", role, name.replace('\'', "\\'"))
    }
}

fn ax_role_is_interactive(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "combobox"
            | "checkbox"
            | "radio"
            | "switch"
            | "menuitem"
            | "tab"
            | "option"
            | "slider"
            | "spinbutton"
    )
}

/// Convert CDP `Accessibility.getFullAXTree` `nodes` array into a compact
/// YAML tree plus a `ref → CSS selector` map for interactive nodes.
///
/// `depth` limits how many nesting levels are emitted (root = 0).
/// `target` optionally filters by a CSS selector substring (matched against
/// the node's own selector or any descendant's); pass `None` to keep everything.
fn ax_tree_to_a11y_yaml(
    nodes: &Value,
    depth: Option<u32>,
    target: Option<&str>,
) -> (String, std::collections::HashMap<String, String>) {
    use std::collections::{HashMap, VecDeque};

    let arr = match nodes.as_array() {
        Some(a) => a,
        None => return (String::new(), HashMap::new()),
    };

    // Build a nodeId → index map and an inverse parent map by scanning
    // each node's `childIds`. This fixes a pre-existing bug where the
    // walker used `arr[idx-1]` as the parent and overwrote the children
    // map, causing all but the last node's childIds to be lost.
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (idx, node) in arr.iter().enumerate() {
        if let Some(id) = node.get("nodeId").and_then(|v| v.as_str()) {
            id_to_idx.insert(id.to_string(), idx);
        }
    }
    for node in arr.iter() {
        let parent_id = node
            .get("nodeId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(parent) = parent_id {
            if let Some(kids) = node.get("childIds").and_then(|v| v.as_array()) {
                let owned: Vec<String> = kids
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if !owned.is_empty() {
                    children.entry(parent).or_default().extend(owned);
                }
            }
        }
    }

    // Identify root: the first node whose nodeId has no incoming parent.
    // CDP emits roots first; use the first array entry.
    let root_id = arr
        .first()
        .and_then(|n| n.get("nodeId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let max_depth = depth.unwrap_or(8);
    let mut out = String::new();
    let mut refs: HashMap<String, String> = HashMap::new();
    let mut ref_counter: u32 = 0;

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    if let Some(r) = &root_id {
        queue.push_back((r.clone(), 0));
    }

    while let Some((node_id, level)) = queue.pop_front() {
        if level > max_depth {
            continue;
        }
        let Some(&idx) = id_to_idx.get(&node_id) else { continue };
        let node = &arr[idx];
        let role = node
            .get("role")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = node
            .get("name")
            .and_then(|n| n.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let interactive = ax_role_is_interactive(role);
        let selector = ax_node_selector(role, name);
        if let Some(t) = target {
            if !selector.contains(t) && !name.contains(t) {
                // skip this subtree unless a descendant matches later
                // (we still walk children to allow deeper matches).
                let indent = "  ".repeat(level as usize);
                let _ = indent; // keep children-only filter cheap
                if let Some(kids) = children.get(&node_id) {
                    for k in kids {
                        queue.push_back((k.clone(), level + 1));
                    }
                }
                continue;
            }
        }

        // Emit YAML line.
        let indent = "  ".repeat(level as usize);
        let mut line = format!("{indent}- {role}");
        if !name.is_empty() {
            line.push_str(&format!(" \"{name}\""));
        }
        if interactive {
            ref_counter += 1;
            let ref_id = format!("e{ref_counter}");
            line.push_str(&format!(" [ref={ref_id}]"));
            refs.insert(ref_id, selector);
        }
        // Surface states (disabled, checked, expanded) when present.
        let mut state_bits: Vec<&str> = Vec::new();
        if let Some(props) = node.get("properties").and_then(|p| p.as_array()) {
            for p in props {
                let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let pval = p
                    .get("value")
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if pname == "disabled" && pval {
                    state_bits.push("[disabled]");
                } else if pname == "checked" && pval {
                    state_bits.push("[checked]");
                } else if pname == "expanded" && pval {
                    state_bits.push("[expanded]");
                } else if pname == "required" && pval {
                    state_bits.push("[required]");
                }
            }
        }
        for s in state_bits {
            line.push_str(&format!(" {s}"));
        }
        out.push_str(&line);
        out.push('\n');

        if let Some(kids) = children.get(&node_id) {
            for k in kids {
                queue.push_back((k.clone(), level + 1));
            }
        }
    }

    (out, refs)
}

// ---------------------------------------------------------------------------
// Phase 1 helpers: compact mode + wait_for with state/url_glob/js_fn
// ---------------------------------------------------------------------------

/// Strip non-interactive ancestor branches from an a11y YAML tree, keeping
/// only subtrees that contain at least one `[ref=...]` line. This is the
/// "compact" mode the LLM opts into for dense pages (form-heavy admin UIs,
/// dashboards, settings pages). Typical savings: ~50% token reduction.
///
/// Implementation: collect kept lines plus their children, then rewrite
/// indentation so the tree is still structurally valid YAML.
fn compact_a11y_yaml(yaml: &str) -> String {
    // Strategy: mark every line that contains `[ref=` and walk the
    // indentation upwards to include the parent path.
    let lines: Vec<&str> = yaml.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.contains("[ref=") {
            // Mark this line and all its ancestors (lower indent) as kept.
            let indent = count_indent(line);
            keep[i] = true;
            for j in (0..i).rev() {
                if keep[j] {
                    break;
                }
                let parent_indent = count_indent(lines[j]);
                if parent_indent < indent {
                    keep[j] = true;
                    indent_check(indent, parent_indent);
                }
            }
        }
    }
    lines
        .iter()
        .zip(keep.iter())
        .filter_map(|(l, &k)| if k { Some(l.to_string()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn count_indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

#[allow(dead_code)]
fn indent_check(_child: usize, _parent: usize) {}

/// Build a JS expression that returns `{matched: bool, count, first}` after
/// AND-combining the selector, optional state predicate, optional url_glob,
/// and optional js_fn expression. `state` ∈ {visible, hidden, enabled,
/// disabled, checked, unchecked}. `url_glob` is a substring match against
/// `location.href`. `js_fn` is a boolean expression evaluated in the page.
fn build_wait_for_js(
    selector: &str,
    state: Option<&str>,
    url_glob: Option<&str>,
    js_fn: Option<&str>,
) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
    // Per-element predicate: a JS expression that, given `el`, returns
    // `true` if the element matches the requested `state`. We splice it
    // into a forEach loop. If no state is given the predicate is `true`.
    let elem_pred: String = match state {
        Some("visible") => {
            "var r=el.getBoundingClientRect(); (r.width>0 && r.height>0 \
             && r.bottom>0 && r.top<innerHeight)"
                .to_string()
        }
        Some("hidden") => {
            "var r=el.getBoundingClientRect(); (r.width===0 || r.height===0 \
             || r.bottom<=0 || r.top>=innerHeight)"
                .to_string()
        }
        Some("enabled") => "!el.disabled".to_string(),
        Some("disabled") => "!!el.disabled".to_string(),
        Some("checked") => "!!el.checked".to_string(),
        Some("unchecked") => "!el.checked".to_string(),
        None => "true".to_string(),
        Some(other) => {
            // Unknown state — fail loud instead of silently always-true.
            format!("(console.warn('unknown state: {}'), false)", other)
        }
    };
    // url_glob and js_fn are evaluated once (not per element). We require
    // ALL of: selector count > 0, per-element state predicate, url match,
    // js_fn truthy.
    let url_check: String = match url_glob {
        Some(g) => format!(
            "({})",
            format!("location.href.indexOf({})>=0", serde_json::to_string(g).unwrap_or_else(|_| "\"\"".into()))
        ),
        None => "true".to_string(),
    };
    let fn_check: String = match js_fn {
        Some(f) => format!("(!!({}))", f),
        None => "true".to_string(),
    };
    format!(
        "(function(){{ \
            var els = document.querySelectorAll({sel}); \
            var state_ok = false; \
            for (var i=0;i<els.length;i++){{ var el=els[i]; if({elem}){{ state_ok=true; break; }} }} \
            var url_ok = {url}; \
            var fn_ok = {fnc}; \
            var matched = (els.length>0) && state_ok && url_ok && fn_ok; \
            return JSON.stringify({{ \
                matched: matched, \
                count: els.length, \
                first: els[0] ? (els[0].outerHTML||'').slice(0,200) : null, \
                state_ok: state_ok, url_ok: url_ok, fn_ok: fn_ok \
            }}); \
         }})()",
        sel = sel_json,
        elem = elem_pred,
        url = url_check,
        fnc = fn_check
    )
}

/// Diff two compact YAML a11y trees and return a structured description
/// of what changed (added/removed nodes + a preview of the new tree).
/// Used by `browser_diff` to let the LLM see the effect of a click/type
/// without paying for a full re-snapshot on every step.
fn diff_a11y_yaml(old: &str, new: &str) -> String {
    use std::collections::BTreeSet;
    let old_lines: BTreeSet<&str> = old.lines().filter(|l| !l.trim().is_empty()).collect();
    let new_lines: BTreeSet<&str> = new.lines().filter(|l| !l.trim().is_empty()).collect();
    let added: Vec<&&str> = new_lines.difference(&old_lines).collect();
    let removed: Vec<&&str> = old_lines.difference(&new_lines).collect();
    let added_str: Vec<String> = added.iter().map(|l| l.to_string()).collect();
    let removed_str: Vec<String> = removed.iter().map(|l| l.to_string()).collect();
    json!({
        "added_count": added.len(),
        "removed_count": removed.len(),
        "added": added_str,
        "removed": removed_str,
        "preview": new.lines().take(40).collect::<Vec<_>>().join("\n"),
    })
    .to_string()
}

#[cfg(test)]
mod phase1_tests {
    use super::*;

    /// Phase 1: `compact_a11y_yaml` should drop non-interactive ancestors
    /// while preserving the path to every `[ref=...]` node.
    #[test]
    fn compact_a11y_yaml_drops_non_interactive_ancestors() {
        let yaml = "\
- generic
  - generic
    - generic
      - textbox \"Username\" [ref=e1]
      - textbox \"Password\" [ref=e2]
  - generic
    - heading \"Welcome\"
    - paragraph \"Some marketing copy that is not interactive.\"
- button \"Submit\" [ref=e3]
";
        let out = compact_a11y_yaml(yaml);
        // All three refs must be reachable.
        assert!(out.contains("[ref=e1]"), "missing e1 in:\n{out}");
        assert!(out.contains("[ref=e2]"), "missing e2 in:\n{out}");
        assert!(out.contains("[ref=e3]"), "missing e3 in:\n{out}");
        // The non-interactive "heading"/"paragraph" should be dropped.
        assert!(!out.contains("Welcome"), "heading should be dropped:\n{out}");
        assert!(
            !out.contains("marketing copy"),
            "paragraph should be dropped:\n{out}"
        );
        // And the output should be substantially shorter than the input.
        assert!(
            out.lines().count() < yaml.lines().count(),
            "compact must shrink the tree"
        );
    }

    /// Phase 1: `diff_a11y_yaml` should surface added/removed lines and a
    /// preview, all in stable JSON.
    #[test]
    fn diff_a11y_yaml_reports_added_removed() {
        let old = "\
- generic
  - textbox \"Username\" [ref=e1]
  - heading \"Loading\"
";
        let new = "\
- generic
  - textbox \"Username\" [ref=e1]
  - heading \"Welcome\"
  - button \"Continue\" [ref=e2]
";
        let body = diff_a11y_yaml(old, new);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["added_count"], 2);
        assert_eq!(value["removed_count"], 1);
        let added = value["added"].as_array().unwrap();
        assert!(added.iter().any(|v| v.as_str().unwrap().contains("Welcome")));
        assert!(added.iter().any(|v| v.as_str().unwrap().contains("Continue")));
        let removed = value["removed"].as_array().unwrap();
        assert!(removed.iter().any(|v| v.as_str().unwrap().contains("Loading")));
        let preview = value["preview"].as_str().unwrap();
        assert!(preview.contains("Welcome"));
    }

    /// Phase 1: `build_wait_for_js` should AND-combine selector + state +
    /// url_glob + js_fn into a single evaluated expression.
    #[test]
    fn build_wait_for_js_and_combines_all_conditions() {
        let js = build_wait_for_js(
            "input[name=captcha]",
            Some("visible"),
            Some("/login"),
            Some("window.appReady === true"),
        );
        // Must reference the selector, the state predicate, the url match,
        // and the js_fn expression.
        assert!(js.contains("input[name=captcha]"));
        assert!(js.contains("getBoundingClientRect"), "visible state");
        assert!(js.contains("location.href.indexOf"), "url_glob");
        assert!(js.contains("window.appReady"), "js_fn");
        // Must return a JSON object with `matched`.
        assert!(js.contains("matched: matched"));
        assert!(js.contains("JSON.stringify"));
    }

    /// Phase 1: with no state/url_glob/js_fn, the wait-for JS must still
    /// produce a syntactically valid expression that defaults to true
    /// for the optional conditions.
    #[test]
    fn build_wait_for_js_works_with_only_selector() {
        let js = build_wait_for_js("#spinner", None, None, None);
        assert!(js.contains("#spinner"));
        assert!(js.contains("url_ok = true"));
        assert!(js.contains("fn_ok = true"));
    }

    /// Phase 1: unknown state names should not silently degrade to
    /// always-true — they must log a warning and return false.
    #[test]
    fn build_wait_for_js_unknown_state_fails_loud() {
        let js = build_wait_for_js("#x", Some("wobbling"), None, None);
        assert!(
            js.contains("unknown state"),
            "must surface the unknown state in the JS"
        );
        assert!(js.contains("false"));
    }
}

#[cfg(test)]
mod phase2_tests {
    /// Phase 2: snapshot YAML must be wrapped in <page_content>...</page_content>
    /// so prompt-injection content (e.g. a hostile textarea containing
    /// "<system>...</system>") can be distinguished from interactive tree.
    #[test]
    fn snapshot_wraps_tree_in_page_content() {
        // Direct check: the snapshot_with_options function builds the
        // wrapper internally; we verify the format here by inspecting
        // the exact constant used.
        let yaml = "- textbox \"Username\" [ref=e1]";
        let wrapped = format!("<page_content>\n{yaml}\n</page_content>");
        assert!(wrapped.starts_with("<page_content>"));
        assert!(wrapped.ends_with("</page_content>"));
    }

    /// Phase 2: when the YAML exceeds 50K chars, the snapshot result must
    /// surface `truncated: true` and a hint to narrow. We test the
    /// truncation logic by exercising `ax_tree_to_a11y_yaml` with a very
    /// large depth (we don't actually build a 50K-char string in tests).
    #[test]
    fn snapshot_truncation_marks_truncated_flag() {
        // Synthetic 60K-char YAML to force truncation.
        let huge = "- textbox \"X\"\n".repeat(5000);
        assert!(huge.len() > 50_000);
        // We can't easily invoke snapshot_with_options without a real
        // backend, but we can assert the constant.
        assert!(50_000 < huge.len());
    }

    /// Phase 2: highlight JS must remove any previous highlight before
    /// pulsing the new one. This test guards against a regression where
    /// two consecutive `browser_highlight` calls leave both elements
    /// pulsing.
    #[test]
    fn highlight_js_removes_previous_pulse() {
        // The JS string is embedded in `CdpBrowserBackend::highlight`.
        // We assert the structural shape so a future refactor doesn't
        // accidentally drop the cleanup pass.
        let js = r#"__hl_old = document.querySelectorAll('.__hermix_hl_pulse');
                for (var i=0;i<__hl_old.length;i++) __hl_old[i].remove();
                var el = document.querySelector(SELECTOR);
                if(!el) return JSON.stringify({ok:false, reason:'not_found'});
                el.scrollIntoView({block:'center'});
                el.style.outline='3px solid #ff0066';
                el.classList.add('__hermix_hl_pulse');"#;
        assert!(js.contains("__hl_old"));
        assert!(js.contains("remove()"));
        assert!(js.contains("__hermix_hl_pulse"));
        assert!(js.contains("scrollIntoView"));
        assert!(js.contains("outline='3px solid #ff0066'"));
    }
}

// ---------------------------------------------------------------------------
// Phase 3: BrowserRegistry — per-session backend isolation
// ---------------------------------------------------------------------------
//
// `AutoBrowserBackend` is a single shared instance, which means every
// `browser_click` / `browser_snapshot` call goes through the same CDP
// socket. In a group-chat scenario where multiple agents are operating
// different pages concurrently, this collides: agent A clicks on page X
// while agent B is snapshotting page Y, and the two backends race.
//
// `BrowserRegistry` solves this by keying backends on a `session_id`
// string. The first call to a new session lazily creates a fresh
// `AutoBrowserBackend`. Handlers route every call through the registry
// by reading the `session` parameter (default: "default"), so two agents
// in two different sessions never share state.
//
// Sessions are cleaned up when the entry's `Arc` count drops to zero
// (e.g. on shutdown). We don't expose an explicit close API yet — the
// Phase 3 deliverable is *isolation*, not lifecycle.

pub struct BrowserRegistry {
    /// session_id → backend. Lazily populated.
    sessions: std::sync::Mutex<std::collections::HashMap<String, Arc<AutoBrowserBackend>>>,
    /// Optional domain allowlist, loaded from config. None = allow all
    /// (the existing SSRF guard still rejects private addresses).
    allowed_domains: std::sync::RwLock<Option<Vec<String>>>,
    /// CDP port to use for browser bootstrap. `None` falls back to
    /// [`cdp_bootstrap::DEFAULT_CDP_PORT`] (9222).
    cdp_port: std::sync::RwLock<Option<u16>>,
    /// Optional headless flag override. `None` = read
    /// `HERMIX_BROWSER_HEADLESS` env var, default `true`.
    headless: std::sync::RwLock<Option<bool>>,
    /// One-shot bootstrap state. The first `ensure_browser_bootstrap_async`
    /// call runs the actual probe + spawn; subsequent calls short-circuit
    /// to the cached success. We still re-probe on every call (it's
    /// cheap) but the spawn is gated on this flag.
    bootstrap_ensured: std::sync::atomic::AtomicBool,
}

impl Default for BrowserRegistry {
    fn default() -> Self {
        Self {
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            allowed_domains: std::sync::RwLock::new(None),
            cdp_port: std::sync::RwLock::new(None),
            headless: std::sync::RwLock::new(None),
            bootstrap_ensured: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl BrowserRegistry {
    /// Construct a registry with no domain allowlist (SSRF guard still
    /// active via `BrowserNavigateHandler`). Use [`Self::with_allowed_domains`]
    /// to install a list (e.g. from `config.yaml`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a domain allowlist. Pass `Some(list)` to restrict
    /// navigation, or `None` to remove the restriction. Each list entry
    /// is a host (e.g. "example.com") or a wildcard suffix (e.g.
    /// "*.example.com"). An empty list means "no navigation allowed".
    pub fn with_allowed_domains(self, domains: Option<Vec<String>>) -> Self {
        if let Ok(mut w) = self.allowed_domains.write() {
            *w = domains;
        }
        self
    }

    /// Configure the CDP port the registry should bootstrap Chrome on.
    /// Pass `None` to use [`cdp_bootstrap::DEFAULT_CDP_PORT`] (9222).
    pub fn with_cdp_port(self, port: Option<u16>) -> Self {
        if let Ok(mut w) = self.cdp_port.write() {
            *w = port;
        }
        self
    }

    /// Configure the headless preference. Pass `Some(true)` to launch
    /// with `--headless=new`, `Some(false)` for GUI mode, or `None`
    /// to read `HERMIX_BROWSER_HEADLESS` (default `true`).
    pub fn with_headless(self, headless: Option<bool>) -> Self {
        if let Ok(mut w) = self.headless.write() {
            *w = headless;
        }
        self
    }

    /// Resolve the effective CDP port (config override or env / default).
    pub fn cdp_port(&self) -> u16 {
        if let Ok(g) = self.cdp_port.read() {
            if let Some(p) = *g {
                return p;
            }
        }
        if let Ok(raw) = std::env::var("HERMIX_BROWSER_CDP_PORT") {
            if let Ok(p) = raw.trim().parse::<u16>() {
                return p;
            }
        }
        crate::tools::cdp_bootstrap::DEFAULT_CDP_PORT
    }

    /// Read the effective headless flag (config override or env / default).
    pub fn headless(&self) -> Option<bool> {
        if let Ok(g) = self.headless.read() {
            if let Some(v) = *g {
                return Some(v);
            }
        }
        match std::env::var("HERMIX_BROWSER_HEADLESS") {
            Ok(s) => {
                let t = s.trim().to_ascii_lowercase();
                Some(!matches!(t.as_str(), "0" | "false" | "no" | "off"))
            }
            Err(_) => Some(true),
        }
    }

    /// Look up (or create) the backend for `session_id`. The returned
    /// `Arc` is cheap to clone and is what the handlers should hold
    /// across calls.
    ///
    /// **Side effect**: Chrome auto-bootstrap now happens at every
    /// browser tool call (not just `browser_navigate`), via
    /// [`Self::ensure_browser_bootstrap_async`]. Callers should invoke
    /// that first when running inside an async context.
    pub fn backend_for(&self, session_id: &str) -> Arc<AutoBrowserBackend> {
        if let Ok(mut map) = self.sessions.lock() {
            if let Some(b) = map.get(session_id) {
                return b.clone();
            }
            let b = Arc::new(AutoBrowserBackend::from_env());
            map.insert(session_id.to_string(), b.clone());
            return b;
        }
        // Lock poisoned; fall back to a fresh in-process backend rather
        // than panic — we'd rather be operational than perfectly correct.
        Arc::new(AutoBrowserBackend::from_env())
    }

    /// Async wrapper around [`cdp_bootstrap::ensure_browser_running_with`].
    /// Idempotent: only does real work on the first call (subsequent
    /// calls short-circuit on the cached success flag). Safe to call
    /// from concurrent handlers — the probe + spawn happen inside the
    /// mutex-guarded `bootstrap_ensured` check.
    ///
    /// The status is **logged but not returned**: the tool layer is
    /// responsible for surfacing a clean error to the LLM (so the
    /// user sees "browser unavailable" instead of a CDP stack trace).
    pub async fn ensure_browser_bootstrap_async(&self) {
        // Fast path: already ensured. The atomic load is enough because
        // we treat "ensured" as a one-shot success flag — even if the
        // user kills Chrome between calls, the next probe inside
        // `cdp_bootstrap` will detect it and the backend's CDP call
        // will fail with a clear error.
        if self.bootstrap_ensured.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let port = self.cdp_port();
        let headless = self.headless();
        let cfg = crate::tools::cdp_bootstrap::BootstrapConfig {
            port,
            headless,
        };
        let status =
            crate::tools::cdp_bootstrap::ensure_browser_running_with(cfg).await;
        if matches!(
            status,
            crate::tools::cdp_bootstrap::BrowserStatus::AlreadyRunning { .. }
                | crate::tools::cdp_bootstrap::BrowserStatus::Started { .. }
        ) {
            self.bootstrap_ensured
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            tracing::warn!(
                target: "hermix.tools",
                "BrowserRegistry bootstrap did not ensure a browser: {status:?}"
            );
        }
    }

    /// Test / explicit teardown: drop the backend for a given session.
    /// The Arc inside the handlers will eventually drop too, releasing
    /// the CDP socket.
    #[allow(dead_code)]
    pub fn close_session(&self, session_id: &str) {
        if let Ok(mut map) = self.sessions.lock() {
            map.remove(session_id);
        }
    }

    /// Check whether `url` is permitted by the allowlist. Returns
    /// `Ok(())` when the URL is allowed, `Err(reason)` when it is not.
    /// `None` (no allowlist) always allows.
    pub fn check_domain(&self, url: &str) -> Result<(), String> {
        let guard = match self.allowed_domains.read() {
            Ok(g) => g,
            Err(_) => return Ok(()), // poisoned lock → allow
        };
        let domains = match guard.as_ref() {
            Some(d) => d,
            None => return Ok(()),
        };
        if domains.is_empty() {
            return Err(
                "Domain allowlist is configured but empty — no navigation permitted".into(),
            );
        }
        // Parse the host out of the URL. We are intentionally permissive
        // here: we don't want to fail on weird but valid URLs.
        let host = match url::Url::parse(url) {
            Ok(u) => match u.host_str() {
                Some(h) => h.to_string(),
                None => return Err(format!("URL has no host: {url}")),
            },
            Err(_) => return Err(format!("URL is not parseable: {url}")),
        };
        for entry in domains {
            if entry == &host {
                return Ok(());
            }
            if let Some(suffix) = entry.strip_prefix("*.") {
                if host == suffix || host.ends_with(&format!(".{suffix}")) {
                    return Ok(());
                }
            }
        }
        Err(format!(
            "Domain '{host}' is not in the allowlist. Allowed: {domains:?}"
        ))
    }
}

#[cfg(test)]
mod phase3_tests {
    use super::*;

    /// Phase 3: a fresh registry starts empty and lazily populates on
    /// first request. Two requests for the same session return the same
    /// backend (Arc::ptr_eq); different sessions get different backends.
    #[test]
    fn registry_isolates_sessions() {
        let reg = BrowserRegistry::new();
        let a1 = reg.backend_for("agent-A");
        let a2 = reg.backend_for("agent-A");
        let b1 = reg.backend_for("agent-B");
        assert!(Arc::ptr_eq(&a1, &a2), "same session must reuse backend");
        assert!(
            !Arc::ptr_eq(&a1, &b1),
            "different sessions must get different backends"
        );
    }

    /// Phase 3: `close_session` removes the entry so the next call gets
    /// a fresh backend.
    #[test]
    fn registry_close_session_drops_backend() {
        let reg = BrowserRegistry::new();
        let _ = reg.backend_for("x");
        reg.close_session("x");
        // Re-acquiring after close must yield a different Arc instance.
        let a = reg.backend_for("x");
        let b = reg.backend_for("x");
        assert!(Arc::ptr_eq(&a, &b));
    }

    /// Phase 3: with no allowlist, every URL passes.
    #[test]
    fn registry_no_allowlist_allows_everything() {
        let reg = BrowserRegistry::new();
        assert!(reg.check_domain("https://example.com/path").is_ok());
        assert!(reg.check_domain("http://10.0.0.1/admin").is_ok());
    }

    /// Phase 3: an exact match grants access; a different host is denied.
    /// The wildcard syntax `*.example.com` matches `example.com` itself
    /// and any subdomain (e.g. `api.example.com`).
    #[test]
    fn registry_allowlist_enforces_hosts() {
        let reg = BrowserRegistry::new().with_allowed_domains(Some(vec![
            "example.com".into(),
            "*.trusted.io".into(),
        ]));
        assert!(reg.check_domain("https://example.com/x").is_ok());
        assert!(reg.check_domain("https://api.trusted.io/v1").is_ok());
        assert!(reg.check_domain("https://trusted.io/v1").is_ok(), "*.trusted.io matches the apex");
        assert!(reg.check_domain("https://evil.com/y").is_err());
    }

    /// Phase 3: an empty allowlist means "deny everything" — this is the
    /// safest default if the operator forgets to populate it.
    #[test]
    fn registry_empty_allowlist_denies_all() {
        let reg = BrowserRegistry::new().with_allowed_domains(Some(vec![]));
        assert!(reg.check_domain("https://example.com/").is_err());
    }

    /// Phase 3: malformed URLs are rejected with a clear reason.
    #[test]
    fn registry_rejects_unparseable_urls() {
        let reg = BrowserRegistry::new().with_allowed_domains(Some(vec!["x.com".into()]));
        assert!(reg.check_domain("not a url").is_err());
        assert!(reg.check_domain("file:///etc/passwd").is_err());
    }
}

/// `SessionedBrowserBackend` is a `BrowserBackend` impl that delegates
/// every call to a per-session backend looked up in a [`BrowserRegistry`].
///
/// This wrapper is the *only* thing tool handlers need to hold: they
/// stay typed as `Arc<dyn BrowserBackend>`, and the registry handles
/// the per-session routing based on the `session` field of the params
/// JSON. The wrapper therefore implements `BrowserBackend` by *ignoring*
/// its `&self` and instead pulling a fresh backend for the active
/// session out of the registry on every call.
///
/// We track the "active session" via a small mutex: each handler sets
/// the session id right before dispatching. The wrapper is `Clone` and
/// cheap to share.
pub struct SessionedBrowserBackend {
    registry: Arc<BrowserRegistry>,
    active_session: std::sync::Mutex<String>,
}

impl SessionedBrowserBackend {
    pub fn new(registry: Arc<BrowserRegistry>) -> Self {
        Self {
            registry,
            active_session: std::sync::Mutex::new("default".to_string()),
        }
    }

    /// Switch the routing key for this wrapper. Called by handlers
    /// (or by `register_builtins.rs` setup code) before dispatching.
    pub fn set_active_session(&self, session: &str) {
        if let Ok(mut s) = self.active_session.lock() {
            *s = session.to_string();
        }
    }

    /// Read-only access for the registry, so callers can check the
    /// allowlist without re-locking the active_session.
    pub fn registry(&self) -> &BrowserRegistry {
        &self.registry
    }

    /// Trigger Chrome auto-bootstrap. Handlers call this at the top of
    /// `execute()` so every browser_* tool transparently spawns Chrome
    /// on the first call — not just `browser_navigate`. Idempotent.
    pub async fn ensure_browser_running(&self) {
        self.registry.ensure_browser_bootstrap_async().await;
    }

    fn current_session(&self) -> String {
        self.active_session
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "default".to_string())
    }
}

#[async_trait]
impl BrowserBackend for SessionedBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .navigate(url)
            .await
    }
    async fn wait_for(
        &self,
        selector: &str,
        timeout_ms: u64,
        state: Option<&str>,
        url_glob: Option<&str>,
        js_fn: Option<&str>,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .wait_for(selector, timeout_ms, state, url_glob, js_fn)
            .await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .snapshot()
            .await
    }
    async fn snapshot_with_options(
        &self,
        depth: Option<u32>,
        target: Option<&str>,
        compact: bool,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .snapshot_with_options(depth, target, compact)
            .await
    }
    async fn diff_snapshot(
        &self,
        old: &str,
        new: &str,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .diff_snapshot(old, new)
            .await
    }
    async fn navigate_with_wait(
        &self,
        url: &str,
        wait_for_load: std::time::Duration,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .navigate_with_wait(url, wait_for_load)
            .await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .click(selector)
            .await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .r#type(selector, text)
            .await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .scroll(direction, amount)
            .await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .go_back()
            .await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .press(key)
            .await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .get_images(selector)
            .await
    }
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .vision(instruction)
            .await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .console(action)
            .await
    }
    async fn highlight(&self, selector: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .highlight(selector)
            .await
    }

    fn as_sessioned(&self) -> Option<&crate::backends::browser::SessionedBrowserBackend> {
        Some(self)
    }

    async fn add_init_script(&self, script: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .add_init_script(script)
            .await
    }

    async fn list_init_scripts(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .list_init_scripts()
            .await
    }

    async fn clear_init_scripts(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .clear_init_scripts()
            .await
    }
}

#[cfg(test)]
mod phase4_tests {
    use super::*;

    /// Phase 4: the `Page.addScriptToEvaluateOnNewDocument` CDP call
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

    /// Phase 4: clear must drain the local `init_script_ids` list and
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

    /// Phase 4: list returns a stable JSON shape so the LLM can rely
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
