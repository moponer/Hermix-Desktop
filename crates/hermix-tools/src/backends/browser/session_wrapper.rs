//! `SessionedBrowserBackend` — the per-session routing wrapper.
//!
//! This wrapper is the *only* thing tool handlers need to hold: they
//! stay typed as `Arc<dyn BrowserBackend>`, and the registry handles
//! the per-session routing based on the `session` field of the params
//! JSON. The wrapper implements `BrowserBackend` by *ignoring* its
//! `&self` and instead pulling a fresh backend for the active session
//! out of the registry on every call. We track the "active session" via
//! a small mutex: each handler sets the session id right before
//! dispatching. The wrapper is `Clone` and cheap to share.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::browser::BrowserBackend;
use hermix_core::ToolError;

use super::registry::BrowserRegistry;

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

    async fn evaluate_js(&self, expression: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(&self.current_session())
            .evaluate_js(expression)
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
