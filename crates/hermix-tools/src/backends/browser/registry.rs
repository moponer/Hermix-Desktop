//! Per-session browser backend isolation.
//!
//! `AutoBrowserBackend` is a single shared instance, which means every
//! `browser_click` / `browser_snapshot` call goes through the same CDP
//! socket. In a group-chat scenario where multiple agents are operating
//! different pages concurrently, this collides: agent A clicks on page X
//! while agent B is snapshotting page Y, and the two backends race.
//! `BrowserRegistry` solves this by keying backends on a `session_id`
//! string. The first call to a new session lazily creates a fresh
//! `AutoBrowserBackend`. Handlers route every call through the registry
//! by reading the `session` parameter (default: "default"), so two agents
//! in two different sessions never share state.
//! Sessions are cleaned up when the entry's `Arc` count drops to zero
//! (e.g. on shutdown). We don't expose an explicit close API yet — the
//! deliverable is *isolation*, not lifecycle.

use std::sync::Arc;

use crate::tools::cdp_bootstrap::{
    ensure_browser_running_with, BrowserStatus, BootstrapConfig, DEFAULT_CDP_PORT,
};
use super::cloud::AutoBrowserBackend;

pub struct BrowserRegistry {
    /// session_id → backend. Lazily populated.
    sessions: std::sync::Mutex<std::collections::HashMap<String, Arc<AutoBrowserBackend>>>,
    /// Optional domain allowlist, loaded from config. None = allow all
    /// (the existing SSRF guard still rejects private addresses).
    allowed_domains: std::sync::RwLock<Option<Vec<String>>>,
    /// CDP port to use for browser bootstrap. `None` falls back to
    /// [`DEFAULT_CDP_PORT`] (9222).
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
    /// Pass `None` to use [`DEFAULT_CDP_PORT`] (9222).
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
        DEFAULT_CDP_PORT
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
    /// **Side effect**: Chrome auto-bootstrap now happens at every
    /// browser tool call (not just `browser_navigate`), via
    /// [`Self::ensure_browser_bootstrap_async`]. Callers should invoke
    /// that first when running inside an async context.
    pub fn backend_for(&self, session_id: &str) -> Arc<AutoBrowserBackend> {
        if let Ok(mut map) = self.sessions.lock() {
            if let Some(b) = map.get(session_id) {
                return b.clone();
            }
            // 每个会话的实例独占一个 tab（懒建于首次 CDP 命令）：会话
            // 隔离必须落到 CDP target 层才算数——只隔离 refs 等进程内
            // 状态的话，两个实例仍会挤进"最近导航的 tab"互相覆写页面。
            let b = Arc::new(AutoBrowserBackend::from_env_dedicated_tab());
            map.insert(session_id.to_string(), b.clone());
            return b;
        }
        // Lock poisoned; fall back to a fresh in-process backend rather
        // than panic — we'd rather be operational than perfectly correct.
        Arc::new(AutoBrowserBackend::from_env())
    }

    /// Async wrapper around `cdp_bootstrap::ensure_browser_running_with`.
    /// Idempotent: only does real work on the first call (subsequent
    /// calls short-circuit on the cached success flag). Safe to call
    /// from concurrent handlers — the probe + spawn happen inside the
    /// mutex-guarded `bootstrap_ensured` check.
    /// The status is **logged but not returned**: the tool layer is
    /// responsible for surfacing a clean error to the LLM (so the
    /// user sees "browser unavailable" instead of a CDP stack trace).
    pub async fn ensure_browser_bootstrap_async(&self) {
        // Fast path: already ensured. The atomic load is enough because
        // we treat "ensured" as a one-shot success flag — even if the
        // user kills Chrome between calls, the next probe inside
        // `cdp_bootstrap` will detect it and the backend's CDP call
        // will fail with a clear error.
        if self
            .bootstrap_ensured
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let port = self.cdp_port();
        let headless = self.headless();
        let cfg = BootstrapConfig {
            port,
            headless,
        };
        let status = ensure_browser_running_with(cfg).await;
        if matches!(
            status,
            BrowserStatus::AlreadyRunning { .. } | BrowserStatus::Started { .. }
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
mod tests {
    use super::*;

    /// a fresh registry starts empty and lazily populates on
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

    /// `close_session` removes the entry so the next call gets
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

    /// with no allowlist, every URL passes.
    #[test]
    fn registry_no_allowlist_allows_everything() {
        let reg = BrowserRegistry::new();
        assert!(reg.check_domain("https://example.com/path").is_ok());
        assert!(reg.check_domain("http://10.0.0.1/admin").is_ok());
    }

    /// an exact match grants access; a different host is denied.
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

    /// an empty allowlist means "deny everything" — this is the
    /// safest default if the operator forgets to populate it.
    #[test]
    fn registry_empty_allowlist_denies_all() {
        let reg = BrowserRegistry::new().with_allowed_domains(Some(vec![]));
        assert!(reg.check_domain("https://example.com/").is_err());
    }

    /// malformed URLs are rejected with a clear reason.
    #[test]
    fn registry_rejects_unparseable_urls() {
        let reg = BrowserRegistry::new().with_allowed_domains(Some(vec!["x.com".into()]));
        assert!(reg.check_domain("not a url").is_err());
        assert!(reg.check_domain("file:///etc/passwd").is_err());
    }
}
