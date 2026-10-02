//! Cloud provider backends and the auto-selecting runtime.
//!
//! Three managed providers (browser-use / browserbase / firecrawl) each
//! implement `CloudBrowserProvider` to lazily provision a CDP session; a
//! `CloudBrowserBackendAdapter` caches the resulting `CdpBrowserBackend`.
//! `AutoBrowserBackend` mirrors the Python provider-choice logic and routes
//! every `BrowserBackend` call to the resolved local / camofox / cloud impl.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::tools::browser::BrowserBackend;
use hermix_config::load_config;
use hermix_config::managed_gateway::{
    prefers_gateway, resolve_managed_tool_gateway, ResolveOptions,
};
use hermix_core::ToolError;

use super::cdp::{CamoFoxBrowserBackend, CdpBrowserBackend};

const DEFAULT_BROWSER_USE_BASE_URL: &str = "https://api.browser-use.com/api/v3";
const DEFAULT_BROWSERBASE_BASE_URL: &str = "https://api.browserbase.com";
const DEFAULT_FIRECRAWL_BASE_URL: &str = "https://api.firecrawl.dev";
const DEFAULT_FIRECRAWL_BROWSER_TTL_SECS: u32 = 300;
const DEFAULT_MANAGED_BROWSER_USE_TIMEOUT_MINUTES: u32 = 5;
const DEFAULT_MANAGED_PROXY_COUNTRY_CODE: &str = "us";

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

    async fn navigate_with_wait(
        &self,
        url: &str,
        wait_for_load: std::time::Duration,
    ) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.navigate_with_wait(url, wait_for_load).await,
            ResolvedBrowserBackend::CamoFox(b) => b.navigate_with_wait(url, wait_for_load).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.navigate_with_wait(url, wait_for_load).await?,
                    provider,
                    transport,
                ))
            }
            ResolvedBrowserBackend::Unsupported(msg) => {
                Err(ToolError::ExecutionFailed(msg.clone()))
            }
        }
    }

    async fn evaluate_js(&self, expression: &str) -> Result<String, ToolError> {
        match &self.inner {
            ResolvedBrowserBackend::Local(b) => b.evaluate_js(expression).await,
            ResolvedBrowserBackend::CamoFox(b) => b.evaluate_js(expression).await,
            ResolvedBrowserBackend::Cloud(b) => {
                let (backend, provider, transport) = b.ensure_backend().await?;
                Ok(annotate_result(
                    backend.evaluate_js(expression).await?,
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

pub(crate) fn annotate_result(raw: String, provider: &str, transport: &str) -> String {
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
}
