//! Browser automation tools
//!
//! All browser tools delegate to an injectable `BrowserBackend` trait,
//! allowing different browser implementations (Playwright, Chromium, etc.)
//! to be plugged in by the caller.
//!
//! Session lifecycle:
//! - [`BrowserSessionManager`] wraps the backend and tracks per-session
//!   activity timestamps. Idle sessions are automatically cleaned up
//!   after a configurable timeout.
//! - [`BrowserConfig`] provides configurable settings (timeout, SSRF policy,
//!   CDP endpoint, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::tools::cdp_bootstrap::{self, BrowserStatus};
use crate::tools::url_safety::SsrfGuard;
use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all browser tool handlers.
pub fn register(registry: &ToolRegistry, backend: Arc<dyn BrowserBackend>) {
    // the toolset tag is the *group* name `"browser"`,
    // not the per-tool name. `essential::ESSENTIAL_TOOLSETS` and
    // `toolset_distributions::base_weights` both reference `"browser"`
    // as a single toolset; if each tool tagged itself with its own
    // name (`"browser_navigate"`, `"browser_click"`, ...) the
    // `set_enabled_toolsets` UI toggle and the essential floor would
    // silently miss every browser tool. The handler `name` (first
    // arg) still carries the unique dispatch identifier.
    let register_with = |name: &str, label: &str, risk: RiskLevel, handler: Arc<dyn ToolHandler>| {
        registry.register_with_risk(
            name,
            "browser",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Browser automation tool",
            label,
            None,
            risk,
            false,
        );
    };

    register_with(
        "browser_navigate",
        "🌐",
        RiskLevel::Safe,
        Arc::new(
            BrowserNavigateHandler::new(backend.clone())
                .with_cdp_bootstrap(crate::tools::cdp_bootstrap::DEFAULT_CDP_PORT),
        ),
    );
    register_with(
        "browser_snapshot",
        "📸",
        RiskLevel::Safe,
        Arc::new(BrowserSnapshotHandler::new(backend.clone())),
    );
    register_with(
        "browser_click",
        "🖱️",
        RiskLevel::Safe,
        Arc::new(BrowserClickHandler::new(backend.clone())),
    );
    register_with(
        "browser_type",
        "⌨️",
        RiskLevel::Safe,
        Arc::new(BrowserTypeHandler::new(backend.clone())),
    );
    register_with(
        "browser_scroll",
        "📜",
        RiskLevel::Safe,
        Arc::new(BrowserScrollHandler::new(backend.clone())),
    );
    register_with(
        "browser_back",
        "⬅️",
        RiskLevel::Safe,
        Arc::new(BrowserBackHandler::new(backend.clone())),
    );
    register_with(
        "browser_press",
        "🔘",
        RiskLevel::Safe,
        Arc::new(BrowserPressHandler::new(backend.clone())),
    );
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configurable settings for browser automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    /// Session inactivity timeout (seconds). Sessions idle longer than
    /// this are automatically cleaned up. Default: 120s.
    pub inactivity_timeout: u64,
    /// Per-command timeout (seconds). Default: 30s.
    pub command_timeout: u64,
    /// Whether to allow navigation to private/internal URLs. Default: false.
    pub allow_private_urls: bool,
    /// Optional CDP endpoint URL for connecting to an existing Chrome instance.
    pub cdp_url: Option<String>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            inactivity_timeout: 120,
            command_timeout: 30,
            allow_private_urls: false,
            cdp_url: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Session Manager
// ---------------------------------------------------------------------------

/// Tracks browser session activity and provides automatic cleanup of
/// idle sessions. Wraps a `BrowserBackend` and updates activity
/// timestamps on every operation.
pub struct BrowserSessionManager {
    backend: Arc<dyn BrowserBackend>,
    /// Last activity timestamp per session (session_id -> Instant).
    sessions: Mutex<HashMap<String, Instant>>,
    /// Inactivity timeout before auto-cleanup.
    inactivity_timeout: Duration,
}

impl BrowserSessionManager {
    /// Create a new session manager wrapping the given backend.
    pub fn new(backend: Arc<dyn BrowserBackend>, config: &BrowserConfig) -> Self {
        Self {
            backend,
            sessions: Mutex::new(HashMap::new()),
            inactivity_timeout: Duration::from_secs(config.inactivity_timeout),
        }
    }

    /// Touch the activity timestamp for a session.
    fn touch(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session_id.to_string(), Instant::now());
        }
    }

    /// Remove a session from tracking.
    fn remove(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    /// Find and clean up sessions that have been idle longer than
    /// `inactivity_timeout`. Returns the number of sessions cleaned up.
    pub async fn cleanup_idle(&self) -> usize {
        let now = Instant::now();
        let to_remove: Vec<String> = {
            let sessions = match self.sessions.lock() {
                Ok(s) => s,
                Err(_) => return 0,
            };
            sessions
                .iter()
                .filter(|(_, &last_active)| now.duration_since(last_active) > self.inactivity_timeout)
                .map(|(id, _): (&String, &Instant)| id.clone())
                .collect()
        };

        let count = to_remove.len();
        for session_id in &to_remove {
            tracing::info!("Cleaning up idle browser session: {session_id}");
            self.remove(session_id);
        }
        count
    }

    /// Get the underlying backend reference.
    pub fn backend(&self) -> &Arc<dyn BrowserBackend> {
        &self.backend
    }
}

// ---------------------------------------------------------------------------
// BrowserBackend trait
// ---------------------------------------------------------------------------

/// Injected backend for browser automation operations.
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    async fn navigate(&self, url: &str) -> Result<String, ToolError>;
    /// Wait for a CSS selector to appear in the DOM, polling at 250ms
    /// intervals up to `timeout_ms`. Returns the matched count + a
    /// snippet of the element's outerHTML, or `{ "found": false }` on
    /// timeout. Default implementation falls back to polling via
    /// `Runtime.evaluate`; the CDP backend overrides to use
    /// `DOM.querySelectorAll` and CDP's built-in wait primitives.
    /// `state`, `url_glob`, and `js_fn` extend the wait conditions
    /// .
    /// Backends that don't understand a given condition should ignore
    /// it and fall back to the selector-only check.
    async fn wait_for(
        &self,
        selector: &str,
        timeout_ms: u64,
        state: Option<&str>,
        url_glob: Option<&str>,
        js_fn: Option<&str>,
    ) -> Result<String, ToolError> {
        let _ = (selector, timeout_ms, state, url_glob, js_fn);
        Err(ToolError::ExecutionFailed(
            "wait_for not implemented for this backend".into(),
        ))
    }
    async fn snapshot(&self) -> Result<String, ToolError>;
    /// Compact snapshot with optional `depth` / `target` / `compact` filter.
    /// The default implementation just calls [`Self::snapshot`] for backends
    /// that don't support filtering; CDP-based backends override to produce
    /// a ref-token YAML view.
    /// `compact = true` drops non-interactive ancestor branches, yielding
    /// a flat "form-only" view that uses ~50% fewer tokens on dense pages.
    async fn snapshot_with_options(
        &self,
        depth: Option<u32>,
        target: Option<&str>,
        compact: bool,
    ) -> Result<String, ToolError> {
        let _ = (depth, target, compact);
        self.snapshot().await
    }
    /// Diff two snapshots (old vs new) and return a structured description
    /// of what changed. Used after `click` / `type` / `press` to avoid
    /// re-emitting a full tree on every interaction. Default implementation
    /// just returns the new snapshot (no diff); the CDP backend overrides
    /// with a proper before/after comparison.
    async fn diff_snapshot(
        &self,
        _old_snapshot: &str,
        new_snapshot: &str,
    ) -> Result<String, ToolError> {
        let _ = _old_snapshot;
        Ok(new_snapshot.to_string())
    }
    /// Navigate and wait up to `wait_for_load` for `Page.loadEventFired`.
    /// Default impl falls back to plain `navigate`; CDP backend overrides.
    async fn navigate_with_wait(
        &self,
        url: &str,
        wait_for_load: std::time::Duration,
    ) -> Result<String, ToolError> {
        let _ = wait_for_load;
        self.navigate(url).await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError>;
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError>;
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError>;
    async fn go_back(&self) -> Result<String, ToolError>;
    async fn press(&self, key: &str) -> Result<String, ToolError>;
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError>;
    async fn vision(&self, instruction: &str) -> Result<String, ToolError>;
    async fn console(&self, action: &str) -> Result<String, ToolError>;
    /// visually highlight an element on the page. Default impl
    /// is a no-op (used by cloud / CamoFox backends that don't support
    /// DOM mutation); the CDP backend overrides with a CSS-pulse.
    async fn highlight(&self, _selector: &str) -> Result<String, ToolError> {
        Ok(json!({"status": "highlight_unsupported"}).to_string())
    }
    /// downcast helper. Implementations that wrap a
    /// `SessionedBrowserBackend` override this to return `Some(self)`.
    /// The default returns `None` so non-sessioned backends still
    /// compile and run.
    fn as_sessioned(&self) -> Option<&crate::backends::browser::SessionedBrowserBackend> {
        None
    }
    /// register a JS snippet to run before every page load.
    /// The CDP `Page.addScriptToEvaluateOnNewDocument` call hooks the
    /// script into all subsequent navigations; we keep the returned
    /// `scriptId` so callers can later clear it. Default impl is a
    /// no-op so cloud / CamoFox backends compile and run unchanged.
    async fn add_init_script(&self, _script: &str) -> Result<String, ToolError> {
        Ok(json!({"status": "init_script_unsupported"}).to_string())
    }
    /// list the init-script IDs currently registered with the
    /// backend. Default impl returns an empty list.
    async fn list_init_scripts(&self) -> Result<String, ToolError> {
        Ok(json!({"status": "init_scripts_listed", "scripts": []}).to_string())
    }
    /// clear all init scripts. Default impl is a no-op.
    async fn clear_init_scripts(&self) -> Result<String, ToolError> {
        Ok(json!({"status": "init_scripts_cleared", "cleared": 0}).to_string())
    }
}

/// Pull the `session` field out of a `params` JSON object, defaulting
/// to `"default"` when absent. Centralising this here means every
/// browser tool gets the same routing key with no per-handler copy.
fn session_from_params(params: &Value) -> &str {
    params
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
}

// ---------------------------------------------------------------------------
// BrowserNavigateHandler
// ---------------------------------------------------------------------------

pub struct BrowserNavigateHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
    /// SSRF guard. `Some(_)` enables the pre-check (default in
    /// [`Self::new`]); `None` skips it ([`Self::disabled`], for
    /// tests or trusted internal callers).
    ssrf_guard: Option<SsrfGuard>,
    /// CDP port to ensure is listening before dispatching to backend.
    /// `Some(_)` triggers [`cdp_bootstrap::ensure_browser_running`]
    /// on every call; `None` skips the check (legacy behavior).
    cdp_port: Option<u16>,
}

impl BrowserNavigateHandler {
    /// Create a handler with the default SSRF blocklist enabled.
    /// Refuses to navigate to `http://169.254.169.254/` (cloud
    /// metadata) and friends before the browser backend ever
    /// sees the URL.
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self {
            backend,
            ssrf_guard: Some(SsrfGuard::new()),
            cdp_port: None,
        }
    }

    /// Create a handler that skips the SSRF pre-check. Use only in
    /// tests or trusted internal contexts.
    pub fn disabled(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self {
            backend,
            ssrf_guard: None,
            cdp_port: None,
        }
    }

    /// Enable auto-bootstrap of a browser on every call. The port
    /// defaults to [`DEFAULT_CDP_PORT`] (9222).
    pub fn with_cdp_bootstrap(mut self, port: u16) -> Self {
        self.cdp_port = Some(port);
        self
    }
}

#[async_trait]
impl ToolHandler for BrowserNavigateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;
        //route to the per-session backend before any side
        // effects. We do this by downcasting to `SessionedBrowserBackend`
        // when possible. A non-sessioned backend (legacy / cloud) is
        // still allowed and the call falls through unchanged.
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Ensure Chrome is running *before* the SSRF/allowlist checks
            // so we fail fast with "browser unavailable" rather than
            // wasting time on URL validation when there's nothing to
            // navigate with.
            if self.cdp_port.is_some() {
                s.ensure_browser_running().await;
            }
        } else if let Some(port) = self.cdp_port {
            // Legacy non-sessioned backend (cloud / tests). Fall back
            // to the direct bootstrap path.
            let status = cdp_bootstrap::ensure_browser_running(port).await;
            if !matches!(status, BrowserStatus::AlreadyRunning { .. } | BrowserStatus::Started { .. })
            {
                return cdp_bootstrap::status_to_tool_result(&status);
            }
        }
        // SSRF pre-check (see `web::WebExtractHandler::execute`
        // for the rationale). The browser is the highest-risk
        // vector: a successful navigate to a private address gives
        // the LLM both the cookies/storage of that session and
        // the ability to submit forms.
        if let Some(guard) = &self.ssrf_guard {
            if let Err(v) = guard.check(url) {
                return Err(ToolError::ExecutionFailed(v.to_string()));
            }
        }
        //domain allowlist check via the registry. We do this
        // *after* SSRF so private IPs are still blocked even when the
        // operator forgets to populate the allowlist.
        if let Some(s) = self.backend.as_sessioned() {
            if let Err(reason) = s.registry().check_domain(url) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Domain allowlist rejected navigation: {reason}"
                )));
            }
        }

        self.backend.navigate(url).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to navigate to"
            }),
        );
        props.insert(
            "session".into(),
            json!({
                "type": "string",
                "description": "Session id for per-session backend isolation. \
                                Use distinct ids for parallel agents / group chats. \
                                Defaults to 'default'."
            }),
        );
        tool_schema(
            "browser_navigate",
            "Navigate the browser to a URL. Pass `session` to route this navigation to \
             a per-session backend.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserSnapshotHandler
// ---------------------------------------------------------------------------

pub struct BrowserSnapshotHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserSnapshotHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let depth = params
            .get("depth")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let target = params.get("target").and_then(|v| v.as_str());
        let compact = crate::param_coercion::bool_param(&params, "compact", false);
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend
            .snapshot_with_options(depth, target, compact)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "session".into(),
            json!({
                "type": "string",
                "description": "Session id. Must match the value used in browser_navigate."
            }),
        );
        props.insert(
            "depth".into(),
            json!({
                "type": "integer",
                "description": "Max nesting depth for the YAML tree (root = 0). Default 8. Use 1-3 for large pages."
            }),
        );
        props.insert(
            "target".into(),
            json!({
                "type": "string",
                "description": "Substring filter for role/name; only matching subtrees are emitted."
            }),
        );
        props.insert(
            "compact".into(),
            json!({
                "type": "boolean",
                "description": "Drop non-interactive ancestor branches, keep only subtrees with [ref=] nodes. \
                                ~50% token savings on form-heavy pages. Default false."
            }),
        );
        tool_schema(
            "browser_snapshot",
            "Take a snapshot of the current page as a compact accessibility YAML tree. \
             The output is wrapped in <page_content>...</page_content> so prompt-injection \
             content on the page can be distinguished from real interactive structure. \
             Each interactive node gets a [ref=eN] handle you can pass to browser_click / \
             browser_type instead of writing a CSS selector. Pass `depth` to limit nesting, \
             `target` to filter by name/role substring, `compact` to drop non-interactive branches. \
             Output is hard-capped at 50K characters; if truncated, a hint tells you to narrow.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserDiffHandler
// ---------------------------------------------------------------------------

pub struct BrowserDiffHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserDiffHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserDiffHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let old = params
            .get("old")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'old' parameter".into()))?;
        let new = params
            .get("new")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'new' parameter".into()))?;
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend.diff_snapshot(old, new).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "old".into(),
            json!({
                "type": "string",
                "description": "Previous browser_snapshot result (the `tree` field)."
            }),
        );
        props.insert(
            "new".into(),
            json!({
                "type": "string",
                "description": "Latest browser_snapshot result (the `tree` field)."
            }),
        );
        tool_schema(
            "browser_diff",
            "Diff two browser_snapshot trees. Returns added/removed lines and a preview of \
             the new tree's first 40 lines. Use this after a click/type to see exactly what \
             changed, instead of re-reading the full snapshot.",
            JsonSchema::object(props, vec!["old".into(), "new".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserClickHandler
// ---------------------------------------------------------------------------

pub struct BrowserClickHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserClickHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend.click(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "Element to click — either a CSS selector or a `[ref=eN]` handle from a previous browser_snapshot."
            }),
        );
        props.insert(
            "session".into(),
            json!({
                "type": "string",
                "description": "Session id. Must match the value used in browser_navigate for the same logical agent."
            }),
        );
        tool_schema(
            "browser_click",
            "Click an element on the page. Auto re-snapshots the page so the next ref is up to date.",
            JsonSchema::object(props, vec!["selector".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserTypeHandler
// ---------------------------------------------------------------------------

pub struct BrowserTypeHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserTypeHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserTypeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'text' parameter".into()))?;
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend.r#type(selector, text).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "Element to type into — either a CSS selector or a `[ref=eN]` handle from a previous browser_snapshot."
            }),
        );
        props.insert(
            "text".into(),
            json!({
                "type": "string",
                "description": "Text to type into the element"
            }),
        );
        tool_schema(
            "browser_type",
            "Type text into an element on the page. Auto re-snapshots after input so autocomplete / validation UI shows up in the next snapshot.",
            JsonSchema::object(props, vec!["selector".into(), "text".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserScrollHandler
// ---------------------------------------------------------------------------

pub struct BrowserScrollHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserScrollHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserScrollHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let direction = params
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("down");
        let amount = params
            .get("amount")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        self.backend.scroll(direction, amount).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "direction".into(),
            json!({
                "type": "string",
                "description": "Scroll direction: up, down, left, right",
                "enum": ["up", "down", "left", "right"],
                "default": "down"
            }),
        );
        props.insert(
            "amount".into(),
            json!({
                "type": "integer",
                "description": "Number of pixels to scroll (default: 500)"
            }),
        );
        tool_schema(
            "browser_scroll",
            "Scroll the page in a direction.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserBackHandler
// ---------------------------------------------------------------------------

pub struct BrowserBackHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserBackHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserBackHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        self.backend.go_back().await
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "browser_back",
            "Navigate back in browser history.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserPressHandler
// ---------------------------------------------------------------------------

pub struct BrowserPressHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserPressHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserPressHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
        self.backend.press(key).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "key".into(),
            json!({
                "type": "string",
                "description": "Key to press (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
            }),
        );
        tool_schema(
            "browser_press",
            "Press a keyboard key.",
            JsonSchema::object(props, vec!["key".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserGetImagesHandler
// ---------------------------------------------------------------------------

pub struct BrowserGetImagesHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserGetImagesHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserGetImagesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params.get("selector").and_then(|v| v.as_str());
        self.backend.get_images(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "Optional CSS selector to filter images"
            }),
        );
        tool_schema(
            "browser_get_images",
            "Get images from the current page.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserWaitForHandler
// ---------------------------------------------------------------------------

pub struct BrowserWaitForHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserWaitForHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserWaitForHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000);
        let state = params.get("state").and_then(|v| v.as_str());
        let url_glob = params.get("url_glob").and_then(|v| v.as_str());
        let js_fn = params.get("js_fn").and_then(|v| v.as_str());
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend
            .wait_for(selector, timeout_ms, state, url_glob, js_fn)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector to wait for (e.g. \"form\", \"input[name=captcha]\", \"#login\")."
            }),
        );
        props.insert(
            "timeout_ms".into(),
            json!({
                "type": "integer",
                "description": "How long to poll for the selector in milliseconds. Default 5000. Use 15000 for captcha/slow login pages."
            }),
        );
        props.insert(
            "state".into(),
            json!({
                "type": "string",
                "enum": ["visible", "hidden", "enabled", "disabled", "checked", "unchecked"],
                "description": "Optional element state filter. visible = rect has size and is in viewport, \
                                enabled = not disabled, checked = input[type=checkbox|radio] is checked, etc."
            }),
        );
        props.insert(
            "url_glob".into(),
            json!({
                "type": "string",
                "description": "Optional substring match against window.location.href. \
                                Useful for SPA navigations that don't fire Page.loadEventFired \
                                (e.g. wait for location.href to contain '/dashboard')."
            }),
        );
        props.insert(
            "js_fn".into(),
            json!({
                "type": "string",
                "description": "Optional boolean JS expression evaluated in the page. \
                                E.g. \"window.appReady === true\" or \"document.querySelectorAll('.toast').length > 0\". \
                                All conditions (selector + state + url_glob + js_fn) must be true for the wait to resolve."
            }),
        );
        tool_schema(
            "browser_wait_for",
            "Wait for a CSS selector (plus optional state/url_glob/js_fn) to appear before continuing. \
             Use this when browser_navigate returned `load_observed: false`, when \
             the snapshot is empty, or when the page shows a captcha / spinner. \
             Polls every 250ms up to `timeout_ms`. Returns `{found: true, count, first_html}` \
             on success or `{found: false, hint}` on timeout. Do NOT chain this with \
             web_fetch — it is purely a DOM readiness primitive.",
            JsonSchema::object(props, vec!["selector".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserVisionHandler
// ---------------------------------------------------------------------------

pub struct BrowserVisionHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserVisionHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserVisionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let instruction = params
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'instruction' parameter".into()))?;
        let annotate = crate::param_coercion::bool_param(&params, "annotate", false);
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        let mut out = self.backend.vision(instruction).await?;
        if annotate {
            // Server-side hint: the backend always annotates when ref
            // tokens are present. We just mark it in the response so the
            // LLM knows the badges are visible.
            if let Ok(mut v) = serde_json::from_str::<Value>(&out) {
                v["annotate"] = json!(true);
                out = v.to_string();
            }
        }
        Ok(out)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "instruction".into(),
            json!({
                "type": "string",
                "description": "What to look for or analyze in the current page screenshot"
            }),
        );
        props.insert(
            "annotate".into(),
            json!({
                "type": "boolean",
                "description": "When true, overlay [ref=eN] badges on every interactive element \
                                in the screenshot so you can correlate visual regions to ref tokens. \
                                Strongly recommended for captcha, canvas, map, and chart UIs.",
                "default": false
            }),
        );
        tool_schema(
            "browser_vision",
            "Capture a screenshot of the current page and analyze it via vision. \
             Pass `annotate=true` to overlay [ref=eN] badges on each interactive element \
             (essential for captcha / canvas / chart scenarios).",
            JsonSchema::object(props, vec!["instruction".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserConsoleHandler
// ---------------------------------------------------------------------------

pub struct BrowserConsoleHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserConsoleHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserConsoleHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend.console(action).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Console action. 'read' returns all messages, 'clear' empties the buffer, \
                            'level:<name>' returns only messages at that level (log/warn/error/info/debug).",
            "enum": ["read", "clear", "level:log", "level:warn", "level:error", "level:info", "level:debug"],
            "default": "read"
        }));
        tool_schema(
            "browser_console",
            "Read or clear the browser console (or filter by level). \
             Use 'level:error' to surface only JS errors, 'read' to see everything, \
             'clear' to reset the buffer before a fresh interaction.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserHighlightHandler
// ---------------------------------------------------------------------------
//visually highlight a `[ref=eN]` element on the page with a
// pulsing red border + tooltip. Useful for debugging — "I just clicked
// [ref=e5] but the page didn't change, where is [ref=e5]?". The highlight
// is removed by the next `browser_highlight` call or by `browser_navigate`.

pub struct BrowserHighlightHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserHighlightHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserHighlightHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        self.backend.highlight(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector or [ref=eN] handle to visually highlight."
            }),
        );
        tool_schema(
            "browser_highlight",
            "Visually highlight an element on the page (pulsing red border + auto-scroll into view). \
             Use for debugging — confirms the LLM is targeting the right element when browser_click \
             fails or has unexpected effects.",
            JsonSchema::object(props, vec!["selector".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserInitScriptHandler
// ---------------------------------------------------------------------------
//register a JS snippet that runs before every page load
// (`Page.addScriptToEvaluateOnNewDocument`). Use cases:
// - pre-set `localStorage` / `sessionStorage` / cookies to bypass login
// - stub out anti-bot fingerprinting
// - install a global `window.appReady` event the LLM can `wait_for` on
// The handler supports three actions: `add` (register a script), `list`
// (see what's registered), and `clear` (remove all). Scripts persist
// across navigations on the same backend; they are isolated per session
// by the `SessionedBrowserBackend`.

pub struct BrowserInitScriptHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserInitScriptHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserInitScriptHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("add");
        if let Some(s) = self.backend.as_sessioned() {
            s.set_active_session(session_from_params(&params));
            // Auto-bootstrap Chrome on every browser tool call (not
            // just navigate) so the LLM never sees an opaque
            // "Failed to connect to Chrome CDP" error mid-task.
            s.ensure_browser_running().await;
        }
        match action {
            "add" => {
                let script = params
                    .get("script")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'script' parameter for action=add".into())
                    })?;
                if script.len() > 100_000 {
                    return Err(ToolError::InvalidParams(format!(
                        "Init script too long: {} chars (max 100K)",
                        script.len()
                    )));
                }
                self.backend.add_init_script(script).await
            }
            "list" => self.backend.list_init_scripts().await,
            "clear" => self.backend.clear_init_scripts().await,
            other => Err(ToolError::InvalidParams(format!(
                "Unknown init_script action: '{other}'. Use add / list / clear."
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["add", "list", "clear"],
                "default": "add",
                "description": "Init script action. add = register a script that runs before every page load; \
                                list = show currently registered scripts; clear = remove all."
            }),
        );
        props.insert(
            "script".into(),
            json!({
                "type": "string",
                "description": "JS source to inject before every page load. Required when action=add. Max 100K chars."
            }),
        );
        props.insert(
            "session".into(),
            json!({
                "type": "string",
                "description": "Session id. Scripts are scoped to the session, so two agents can run different init scripts."
            }),
        );
        tool_schema(
            "browser_init_script",
            "Register, list, or clear init scripts that run before every page load (CDP \
             `Page.addScriptToEvaluateOnNewDocument`). Use for login-state pre-set, \
             fingerprinting bypass, or installing a `window.appReady` event for `browser_wait_for(js_fn=...)`.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBrowserBackend;
    #[async_trait]
    impl BrowserBackend for MockBrowserBackend {
        async fn navigate(&self, url: &str) -> Result<String, ToolError> {
            Ok(format!("Navigated to {}", url))
        }
        async fn snapshot(&self) -> Result<String, ToolError> {
            Ok("Page snapshot".into())
        }
        async fn click(&self, sel: &str) -> Result<String, ToolError> {
            Ok(format!("Clicked {}", sel))
        }
        async fn r#type(&self, sel: &str, text: &str) -> Result<String, ToolError> {
            Ok(format!("Typed '{}' into {}", text, sel))
        }
        async fn scroll(&self, dir: &str, _amt: Option<u32>) -> Result<String, ToolError> {
            Ok(format!("Scrolled {}", dir))
        }
        async fn go_back(&self) -> Result<String, ToolError> {
            Ok("Went back".into())
        }
        async fn press(&self, key: &str) -> Result<String, ToolError> {
            Ok(format!("Pressed {}", key))
        }
        async fn get_images(&self, sel: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Images: {:?}", sel))
        }
        async fn vision(&self, inst: &str) -> Result<String, ToolError> {
            Ok(format!("Vision: {}", inst))
        }
        async fn console(&self, action: &str) -> Result<String, ToolError> {
            Ok(format!("Console: {}", action))
        }
    }

    fn backend() -> std::sync::Arc<dyn BrowserBackend> {
        std::sync::Arc::new(MockBrowserBackend)
    }

    #[tokio::test]
    async fn test_browser_navigate() {
        let handler = BrowserNavigateHandler::new(backend());
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(result.contains("example.com"));
    }

    #[tokio::test]
    async fn test_browser_click() {
        let handler = BrowserClickHandler::new(backend());
        let result = handler.execute(json!({"selector": "#btn"})).await.unwrap();
        assert!(result.contains("#btn"));
    }

    #[tokio::test]
    async fn test_browser_snapshot() {
        let handler = BrowserSnapshotHandler::new(backend());
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("snapshot"));
    }

    // -- SSRF pre-check -------------------------------------------------
    // Browser-navigate is the highest-risk surface: a successful
    // navigate gives the LLM both session cookies/storage and
    // the ability to submit forms on the destination. The guard
    // sits in the handler, not the backend, so the policy is
    // enforced regardless of which browser implementation is
    // plugged in.

    #[tokio::test]
    async fn test_browser_navigate_blocks_cloud_metadata() {
        let handler = BrowserNavigateHandler::new(backend());
        let err = handler
            .execute(json!({"url": "http://169.254.169.254/latest/meta-data/"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("link_local"));
    }

    #[tokio::test]
    async fn test_browser_navigate_blocks_loopback() {
        // Navigating to localhost would give the LLM full
        // access to any dev server the user happens to be
        // running on 127.0.0.1, including the desktop app's
        // own IPC endpoints.
        let handler = BrowserNavigateHandler::new(backend());
        let err = handler
            .execute(json!({"url": "http://127.0.0.1:9222/json"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }

    #[tokio::test]
    async fn test_browser_navigate_blocks_file_scheme() {
        let handler = BrowserNavigateHandler::new(backend());
        let err = handler
            .execute(json!({"url": "file:///C:/Windows/System32/drivers/etc/hosts"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("forbidden_scheme"));
    }

    #[tokio::test]
    async fn test_browser_navigate_disabled_bypasses_ssrf() {
        let handler = BrowserNavigateHandler::disabled(backend());
        let result = handler
            .execute(json!({"url": "http://10.0.0.1/admin"}))
            .await
            .unwrap();
        assert!(result.contains("10.0.0.1"));
    }
}
