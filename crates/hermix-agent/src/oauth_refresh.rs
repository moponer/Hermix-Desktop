//! OAuth token refresh for runtime providers.
//!
//! Splits out of `agent_loop.rs` on 2026-07-04 to consolidate the on-disk
//! credential store, the `reqwest` token-exchange POST, and the
//! per-provider backoff window in one module. `OAuthStoreCredential` is
//! the row shape persisted to `auth_tokens.json`; `OAuthRefresh` owns
//! the mutex-guarded `last_failure` map so the agent loop does not
//! hammer token endpoints every turn after a refresh fails.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use chrono::{DateTime, Utc};
use hermix_core::AgentError;
use serde::{Deserialize, Serialize};

use crate::agent_config::RuntimeProviderConfig;

/// One row in the on-disk `auth_tokens.json` store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthStoreCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// Manager for runtime-provider OAuth token refresh + backoff state.
///
/// Owns the per-provider `last_failure` window so the agent loop does not
/// hammer token endpoints every turn after a refresh fails. All call sites
/// delegate through `AgentLoop::oauth_refresh` rather than touching state
/// directly.
#[derive(Debug, Default)]
pub struct OAuthRefresh {
    backoff: Mutex<HashMap<String, Instant>>,
}

impl OAuthRefresh {
    /// Cooldown window after a refresh failure: skip retries within this
    /// many seconds. Prevents tight per-turn hammering of the token endpoint.
    const BACKOFF_SECS: u64 = 60;

    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the access token for a runtime provider from the on-disk
    /// auth-tokens store. Returns `None` if the entry is missing, the token
    /// is blank, or the entry has already expired.
    pub fn resolve_api_key(&self, auth_tokens_path: &Path, provider: &str) -> Option<String> {
        let provider_key = match provider {
            "openai-codex" | "codex" => "openai-codex",
            "qwen-oauth" => "qwen-oauth",
            _ => return None,
        };
        let raw = std::fs::read_to_string(auth_tokens_path).ok()?;
        let entries: HashMap<String, OAuthStoreCredential> = serde_json::from_str(&raw).ok()?;
        let cred = entries.get(provider_key)?;
        if cred.access_token.trim().is_empty() {
            return None;
        }
        if cred
            .expires_at
            .map(|exp| exp <= Utc::now())
            .unwrap_or(false)
        {
            return None;
        }
        Some(cred.access_token.clone())
    }

    /// Refresh all known oauth-backed runtime providers whose tokens are
    /// expired. Honors the per-provider backoff window after recent failures.
    pub async fn refresh_if_needed(
        &self,
        auth_tokens_path: &Path,
        runtime_providers: &HashMap<String, RuntimeProviderConfig>,
    ) {
        // Keep this list explicit so behavior is deterministic and parity-scoped.
        self.refresh_single_if_needed(auth_tokens_path, "openai-codex", runtime_providers)
            .await;
        self.refresh_single_if_needed(auth_tokens_path, "qwen-oauth", runtime_providers)
            .await;
    }

    async fn refresh_single_if_needed(
        &self,
        auth_tokens_path: &Path,
        provider_key: &str,
        runtime_providers: &HashMap<String, RuntimeProviderConfig>,
    ) {
        if !self.can_attempt(provider_key) {
            return;
        }
        let raw = match tokio::fs::read_to_string(auth_tokens_path).await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut entries: HashMap<String, OAuthStoreCredential> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(current) = entries.get(provider_key).cloned() else {
            return;
        };
        let Some(expires_at) = current.expires_at else {
            return;
        };
        if expires_at > Utc::now() {
            return;
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            return;
        };
        let Some((token_url, client_id)) = self.refresh_config(runtime_providers, provider_key)
        else {
            return;
        };
        let refreshed = match self
            .exchange_refresh_token(
                provider_key,
                token_url.as_str(),
                client_id.as_str(),
                refresh_token.as_str(),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.mark_failure(provider_key);
                tracing::warn!(
                    provider = provider_key,
                    error = %e,
                    "oauth token refresh failed for runtime provider"
                );
                return;
            }
        };
        entries.insert(provider_key.to_string(), refreshed);
        let Ok(content) = serde_json::to_string_pretty(&entries) else {
            self.mark_success(provider_key);
            return;
        };
        if let Some(parent) = auth_tokens_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(auth_tokens_path, content).await;
        self.mark_success(provider_key);
    }

    pub(crate) fn refresh_config(
        &self,
        runtime_providers: &HashMap<String, RuntimeProviderConfig>,
        provider_key: &str,
    ) -> Option<(String, String)> {
        // Preferred source: unified provider config centre (runtime_providers).
        let cfg_token_url = runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_token_url.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let cfg_client_id = runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_client_id.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Env fallback — keeps previous behavior working when config centre is empty.
        let (token_url_env, client_id_env) = match provider_key {
            "openai-codex" => (
                "HERMIX_OPENAI_CODEX_OAUTH_TOKEN_URL",
                "HERMIX_OPENAI_CODEX_OAUTH_CLIENT_ID",
            ),
            "qwen-oauth" => ("HERMIX_QWEN_OAUTH_TOKEN_URL", "HERMIX_QWEN_OAUTH_CLIENT_ID"),
            _ => return cfg_token_url.zip(cfg_client_id),
        };
        let token_url = cfg_token_url.or_else(|| {
            std::env::var(token_url_env)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })?;
        let client_id = cfg_client_id.or_else(|| {
            std::env::var(client_id_env)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })?;
        Some((token_url, client_id))
    }

    async fn exchange_refresh_token(
        &self,
        provider_key: &str,
        token_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<OAuthStoreCredential, AgentError> {
        #[derive(Debug, Deserialize)]
        struct OAuthRefreshResponse {
            access_token: String,
            #[serde(default)]
            token_type: Option<String>,
            refresh_token: Option<String>,
            scope: Option<String>,
            expires_in: Option<i64>,
        }

        let client = reqwest::Client::new();
        let pairs = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];
        let resp = client
            .post(token_url)
            .header("Accept", "application/json")
            .form(&pairs)
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(e.to_string()))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::AuthFailed(format!(
                "token refresh failed for {}: {}",
                provider_key, text
            )));
        }
        let body: OAuthRefreshResponse = resp
            .json()
            .await
            .map_err(|e| AgentError::AuthFailed(e.to_string()))?;
        let expires_at = body
            .expires_in
            .map(|s| Utc::now() + chrono::Duration::seconds(s));
        Ok(OAuthStoreCredential {
            provider: Some(provider_key.to_string()),
            access_token: body.access_token,
            refresh_token: body
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            token_type: Some(body.token_type.unwrap_or_else(|| "Bearer".to_string())),
            scope: body.scope,
            expires_at,
        })
    }

    pub fn can_attempt(&self, provider_key: &str) -> bool {
        let Ok(guard) = self.backoff.lock() else {
            return true;
        };
        let Some(last_fail) = guard.get(provider_key) else {
            return true;
        };
        last_fail.elapsed().as_secs() >= Self::BACKOFF_SECS
    }

    pub fn mark_failure(&self, provider_key: &str) {
        if let Ok(mut guard) = self.backoff.lock() {
            guard.insert(provider_key.to_string(), Instant::now());
        }
    }

    pub fn mark_success(&self, provider_key: &str) {
        if let Ok(mut guard) = self.backoff.lock() {
            guard.remove(provider_key);
        }
    }
}

// Unit tests live in the same file so the `can_attempt` /
// `mark_failure` / `mark_success` surface stays testable without
// spinning up the full agent runtime.

#[cfg(test)]
mod tests {
    //! The synchronous `can_attempt` / `mark_failure` / `mark_success`
    //! surface is testable in isolation without tokio or reqwest.
    //!
    //! The backoff state used to live in `AgentLoop::oauth_refresh_backoff`
    //! and was only exercised through the async refresh path. With
    //! `OAuthRefresh` extracted as its own struct, the public surface
    //! is testable here without spinning up the full agent runtime.

    use super::*;

    #[test]
    fn oauth_refresh_can_attempt_when_no_failures_recorded() {
        let r = OAuthRefresh::default();
        assert!(r.can_attempt("openai-codex"));
        assert!(r.can_attempt("qwen-oauth"));
        assert!(r.can_attempt("any-other-provider"));
    }

    #[test]
    fn oauth_refresh_mark_failure_blocks_within_backoff_window() {
        let r = OAuthRefresh::default();
        r.mark_failure("qwen-oauth");
        // Failure recorded just now — still inside the 60s backoff.
        assert!(!r.can_attempt("qwen-oauth"));
        // Other providers remain unaffected.
        assert!(r.can_attempt("openai-codex"));
    }

    #[test]
    fn oauth_refresh_mark_success_clears_block() {
        let r = OAuthRefresh::default();
        r.mark_failure("qwen-oauth");
        assert!(!r.can_attempt("qwen-oauth"));
        r.mark_success("qwen-oauth");
        assert!(r.can_attempt("qwen-oauth"));
    }

    #[test]
    fn oauth_refresh_backoff_is_per_provider() {
        let r = OAuthRefresh::default();
        r.mark_failure("openai-codex");
        r.mark_failure("qwen-oauth");
        assert!(!r.can_attempt("openai-codex"));
        assert!(!r.can_attempt("qwen-oauth"));
        // Clearing one does not affect the other.
        r.mark_success("openai-codex");
        assert!(r.can_attempt("openai-codex"));
        assert!(!r.can_attempt("qwen-oauth"));
    }

    /// Smoke test: config-centre OAuth metadata wins over env fallback, and
    /// env is used when config is empty. Mirrors the Python behaviour of
    /// `resolve_runtime_provider_credentials` where provider config takes
    /// precedence over environment lookup. Lives here (instead of next to
    /// `AgentLoop`) now that `OAuthRefresh` is its own module — no AgentLoop
    /// constructor needed; pass any `HashMap` of provider config.
    #[test]
    fn test_oauth_refresh_config_prefers_provider_config_over_env() {
        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/token".to_string()),
                oauth_client_id: Some("cfg-client".to_string()),
            },
        );
        // An unknown provider reachable only via config (env fallback is gated
        // on known providers, so this exercises the cfg_token_url.zip path).
        runtime_providers.insert(
            "custom-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/custom-token".to_string()),
                oauth_client_id: Some("custom-client".to_string()),
            },
        );

        let r = OAuthRefresh::default();

        // Set conflicting env values — config must win.
        std::env::set_var("HERMIX_QWEN_OAUTH_TOKEN_URL", "https://env.example.com/tok");
        std::env::set_var("HERMIX_QWEN_OAUTH_CLIENT_ID", "env-client");

        let (token_url, client_id) = r
            .refresh_config(&runtime_providers, "qwen-oauth")
            .unwrap();
        assert_eq!(token_url, "https://cfg.example.com/token");
        assert_eq!(client_id, "cfg-client");

        // Unknown-provider path still resolves when config centre supplies both.
        let (token_url, client_id) = r
            .refresh_config(&runtime_providers, "custom-oauth")
            .unwrap();
        assert_eq!(token_url, "https://cfg.example.com/custom-token");
        assert_eq!(client_id, "custom-client");

        std::env::remove_var("HERMIX_QWEN_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMIX_QWEN_OAUTH_CLIENT_ID");
    }
}
