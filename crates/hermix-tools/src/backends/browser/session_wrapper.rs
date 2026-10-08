//! `SessionedBrowserBackend` — per-session browser routing.
//!
//! Every `browser_*` tool shares one instance of this wrapper. It resolves
//! a backend per session id out of [`BrowserRegistry`] — each session gets
//! its own CDP-connected `AutoBrowserBackend`, so two sessions never share
//! a page.
//!
//! Routing is **bound once per call**, not stored on the wrapper: a handler
//! asks for its session's backend before its first `.await` and issues every
//! later step on that handle ([`Self::backend_for`]). A previous design wrote
//! the session id into a mutable slot on this shared wrapper and re-read it
//! at each step; that let a concurrent handler for another session overwrite
//! the slot mid-call, silently routing the rest of the steps into the other
//! session's page. Everything that remained reachable only through that slot
//! now falls back to the `"default"` session, which is the same key a request
//! with no session id resolves to.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::browser::BrowserBackend;
use hermix_core::ToolError;

use super::registry::BrowserRegistry;

/// Session key used when a caller supplies no session id.
const DEFAULT_SESSION: &str = "default";

pub struct SessionedBrowserBackend {
    registry: Arc<BrowserRegistry>,
}

impl SessionedBrowserBackend {
    pub fn new(registry: Arc<BrowserRegistry>) -> Self {
        Self { registry }
    }

    /// Read-only access for the registry, so callers can check the
    /// allowlist without going through `backend_for`.
    pub fn registry(&self) -> &BrowserRegistry {
        &self.registry
    }

    /// Trigger Chrome auto-bootstrap. Handlers call this at the top of
    /// `execute()` so every browser_* tool transparently spawns Chrome
    /// on the first call — not just `browser_navigate`. Idempotent.
    pub async fn ensure_browser_running(&self) {
        self.registry.ensure_browser_bootstrap_async().await;
    }

    fn current_session(&self) -> &str {
        DEFAULT_SESSION
    }

    /// Resolve the backend for `session`.
    ///
    /// Callers that need per-session routing take this handle **once** and
    /// issue every subsequent call on it. Resolving per step from a shared
    /// mutable key is what let two concurrent sessions land on one page.
    pub fn backend_for(&self, session: &str) -> Arc<crate::backends::browser::AutoBrowserBackend> {
        self.registry.backend_for(session)
    }
}

#[async_trait]
impl BrowserBackend for SessionedBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
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
            .backend_for(self.current_session())
            .wait_for(selector, timeout_ms, state, url_glob, js_fn)
            .await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
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
            .backend_for(self.current_session())
            .snapshot_with_options(depth, target, compact)
            .await
    }
    async fn diff_snapshot(
        &self,
        old: &str,
        new: &str,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .diff_snapshot(old, new)
            .await
    }
    async fn navigate_with_wait(
        &self,
        url: &str,
        wait_for_load: std::time::Duration,
    ) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .navigate_with_wait(url, wait_for_load)
            .await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .click(selector)
            .await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .r#type(selector, text)
            .await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .scroll(direction, amount)
            .await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .go_back()
            .await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .press(key)
            .await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .get_images(selector)
            .await
    }
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .vision(instruction)
            .await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .console(action)
            .await
    }
    async fn highlight(&self, selector: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .highlight(selector)
            .await
    }

    async fn evaluate_js(&self, expression: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .evaluate_js(expression)
            .await
    }

    fn as_sessioned(&self) -> Option<&crate::backends::browser::SessionedBrowserBackend> {
        Some(self)
    }

    async fn add_init_script(&self, script: &str) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .add_init_script(script)
            .await
    }

    async fn list_init_scripts(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .list_init_scripts()
            .await
    }

    async fn clear_init_scripts(&self) -> Result<String, ToolError> {
        self.registry
            .backend_for(self.current_session())
            .clear_init_scripts()
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个会话必须拿到自己的后端实例 —— 这是"多会话不串台"的全部前提。
    /// 此前工具层把 session 写进一个共享槽、每次调用再读出来：两个会话
    /// 并发时后写的覆盖先写的，先写那次调用剩下的步骤就落到别人的页面上
    /// （同一 frameId、一个会话的 snapshot 出现另一个会话的 DOM）。
    #[test]
    fn each_session_resolves_to_its_own_backend() {
        let wrapper = SessionedBrowserBackend::new(Arc::new(BrowserRegistry::new()));
        let alpha = wrapper.backend_for("alpha");
        let beta = wrapper.backend_for("beta");
        assert!(
            !Arc::ptr_eq(&alpha, &beta),
            "两个会话必须解析到不同的后端实例"
        );
        // 同一会话必须复用同一个实例，否则页内状态（cookies / init script）会丢。
        assert!(Arc::ptr_eq(&alpha, &wrapper.backend_for("alpha")));
    }

    /// 不带会话 id 的请求落到 default，且始终稳定。
    #[test]
    fn default_session_resolves_stably() {
        let wrapper = SessionedBrowserBackend::new(Arc::new(BrowserRegistry::new()));
        assert!(Arc::ptr_eq(
            &wrapper.backend_for(DEFAULT_SESSION),
            &wrapper.backend_for("default")
        ));
    }
}
