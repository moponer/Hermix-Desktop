//! Runtime LLM provider construction + credential resolution.
//!
//! : extracted from `agent_loop.rs` so the
//! per-provider construction logic (`build_runtime_provider` + 5
//! dependency helpers) lives next to the provider types it instantiates.
//!
//! All six helpers take `&AgentLoop` directly because they read mostly
//! agent-loop state (config, oauth_refresh, primary_credential_pool);
//! moving them off the inherent impl block shrinks `agent_loop.rs`
//! without changing call sites. Method signatures stay identical —
//! the impl methods in `agent_loop.rs` became 1-liner delegates.
//!
//! Module-level free functions keep the per-provider construction
//! logic contained (the giant match on `"openai" | "anthropic" | ...`
//! lives here, not on `AgentLoop`).
//!
//! `TurnRuntimeRoute` was already `pub(crate)` for the cost_guard
//! sibling — see `agent_loop.rs` L540.

use std::path::PathBuf;
use std::sync::Arc;

use hermix_config::env_compat;
use hermix_core::{AgentError, LlmProvider};

use crate::agent_config::{AgentConfig, RuntimeProviderConfig};
use crate::agent_loop::{ApiMode, TurnRuntimeRoute};
use crate::api_bridge::CodexProvider;
use crate::credential_pool::CredentialPool;
use crate::oauth_refresh::OAuthRefresh;
use crate::provider::{AnthropicProvider, GenericProvider, OpenRouterProvider};

/// Compute the absolute path of the OAuth tokens JSON file for this
/// loop. Resolves `<hermix_home>/auth/tokens.json`, falling back to
/// the canonical `hermix_home()` when the config doesn't override the
/// home.
pub(crate) fn auth_tokens_path(config: &AgentConfig) -> PathBuf {
    let hermix_home = config
        .hermix_home
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(hermix_config::paths::hermix_home);
    hermix_home.join("auth").join("tokens.json")
}

/// Resolve the API key for a runtime-routed provider using the
/// layered precedence:
/// 1. Provider-native override (e.g. `copilot-acp` short-circuit).
/// 2. Stored OAuth token (via [`OAuthRefresh::resolve_api_key`]).
/// 3. Explicit key passed by the caller.
/// 4. `api_key_env_override` env var name.
/// 5. `runtime_providers[provider].api_key` (literal or `${ENV}` form).
/// 6. Provider-default env var (`OPENAI_API_KEY`, etc.).
pub(crate) fn resolve_runtime_api_key(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    provider: &str,
    api_key_env_override: Option<&str>,
    explicit_api_key: Option<&str>,
) -> Option<String> {
    if provider == "copilot-acp" {
        return Some("copilot-acp".to_string());
    }
    if let Some(token) = oauth.resolve_api_key(&auth_tokens_path(config), provider) {
        return Some(token);
    }
    if let Some(key) = explicit_api_key.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(key.to_string());
    }
    if let Some(env_name) = api_key_env_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(v) = std::env::var(env_name) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    if let Some(cfg) = config.runtime_providers.get(provider) {
        if let Some(ref key) = cfg.api_key {
            let trimmed = key.trim();
            if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
                if let Ok(v) = std::env::var(env_ref) {
                    if !v.trim().is_empty() {
                        return Some(v);
                    }
                }
            } else if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    let env_var = match provider {
        "openai" | "codex" | "openai-codex" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "qwen" | "qwen-oauth" => "DASHSCOPE_API_KEY",
        "kimi" | "moonshot" => "MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "nous" => "NOUS_API_KEY",
        "copilot" | "copilot-acp" => "GITHUB_COPILOT_TOKEN",
        _ => "",
    };
    if env_var.is_empty() {
        None
    } else {
        std::env::var(env_var).ok().filter(|v| !v.trim().is_empty())
    }
}

/// Resolve the base URL for a runtime-routed provider.
/// Precedence: route override → `runtime_providers[provider].base_url` →
/// provider-specific fallback (`COPILOT_ACP_BASE_URL`, codex/qwen-oauth
/// defaults).
pub(crate) fn resolve_runtime_base_url(
    config: &AgentConfig,
    provider: &str,
    route_base_url: Option<&str>,
) -> Option<String> {
    if let Some(b) = route_base_url.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(b.to_string());
    }
    let cfg_opt: Option<&RuntimeProviderConfig> = config.runtime_providers.get(provider);
    cfg_opt
        .and_then(|c| c.base_url.as_ref())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if provider == "copilot-acp" {
                std::env::var("COPILOT_ACP_BASE_URL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("acp://copilot".to_string()))
            } else if provider == "openai-codex" || provider == "codex" {
                Some("https://api.openai.com/v1".to_string())
            } else if provider == "qwen-oauth" {
                Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string())
            } else {
                None
            }
        })
}

/// Resolve the ACP command + args for a runtime-routed provider.
/// Honors per-provider `runtime_providers[provider].command`/`.args`
/// over the global `acp_command`/`acp_args`. For `copilot-acp`, the
/// default command is `copilot` (resolved via `which::which`) and
/// default args are `--acp --stdio`.
pub(crate) fn resolve_runtime_command_args(
    config: &AgentConfig,
    provider: Option<&str>,
) -> (Option<String>, Vec<String>) {
    let mut command = config
        .acp_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut args: Vec<String> = config
        .acp_args
        .iter()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();

    if let Some(provider) = provider {
        if let Some(cfg) = config.runtime_providers.get(provider) {
            if let Some(cmd) = cfg
                .command
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                command = Some(cmd.to_string());
            }
            if !cfg.args.is_empty() {
                args = cfg
                    .args
                    .iter()
                    .map(|a| a.trim().to_string())
                    .filter(|a| !a.is_empty())
                    .collect();
            }
        }
        if provider == "copilot-acp" {
            if command.is_none() {
                command = env_compat("HERMIX_COPILOT_ACP_COMMAND", "HERMES_COPILOT_ACP_COMMAND")
                    .or_else(|| std::env::var("COPILOT_CLI_PATH").ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("copilot".to_string()));
            }
            if args.is_empty() {
                args = env_compat("HERMIX_COPILOT_ACP_ARGS", "HERMES_COPILOT_ACP_ARGS")
                    .and_then(|raw| shlex::split(raw.trim()))
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| vec!["--acp".to_string(), "--stdio".to_string()]);
            }
            if let Some(cmd) = command.as_deref() {
                if let Ok(resolved) = which::which(cmd) {
                    command = Some(resolved.to_string_lossy().to_string());
                }
            }
        }
    }
    (command, args)
}

/// Build the right `LlmProvider` implementation for the given provider
/// name + model. Pure construction (no I/O); all credential / URL
/// resolution is delegated to [`resolve_runtime_api_key`] /
/// [`resolve_runtime_base_url`].
///
/// `route_base_url` is the route override (None = use provider default).
/// `api_key_env_override` / `explicit_api_key` are passed through to the
/// resolver. `api_mode` defaults to `self.config.api_mode` when None.
/// `credential_pool` is attached to providers that support rotation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_runtime_provider(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    provider: &str,
    model_name: &str,
    route_base_url: Option<&str>,
    api_key_env_override: Option<&str>,
    explicit_api_key: Option<&str>,
    api_mode: Option<&ApiMode>,
    credential_pool: Option<&Arc<CredentialPool>>,
) -> Result<Arc<dyn LlmProvider>, AgentError> {
    let api_key = resolve_runtime_api_key(config, oauth, provider, api_key_env_override, explicit_api_key)
        .ok_or_else(|| {
            AgentError::Config(format!(
                "No API key configured for runtime-routed provider '{}'",
                provider
            ))
        })?;
    let base_url = resolve_runtime_base_url(config, provider, route_base_url);
    let mode = api_mode.unwrap_or(&config.api_mode);

    let provider_obj: Arc<dyn LlmProvider> = match provider {
        "openai" | "codex" | "openai-codex" => {
            if matches!(mode, ApiMode::CodexResponses) {
                let mut p = CodexProvider::new(&api_key).with_model(model_name);
                if let Some(ref url) = base_url {
                    p = p.with_base_url(url.clone());
                }
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            } else {
                let url = base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let mut p = GenericProvider::new(url, &api_key, model_name);
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            }
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name);
            if let Some(url) = &base_url {
                p = p.with_base_url(url);
            }
            if let Some(pool) = credential_pool {
                p = p.with_credential_pool(pool.clone());
            }
            Arc::new(p)
        }
        "openrouter" => {
            let mut p = OpenRouterProvider::new(&api_key).with_model(model_name);
            if let Some(url) = &base_url {
                p = p.with_base_url(url);
            }
            if let Some(pool) = credential_pool {
                p = p.with_credential_pool(pool.clone());
            }
            Arc::new(p)
        }
        "qwen" | "qwen-oauth" => {
            let url = base_url
                .unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "kimi" | "moonshot" => {
            let url = base_url.unwrap_or_else(|| "https://api.moonshot.cn/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "minimax" => {
            let url = base_url.unwrap_or_else(|| "https://api.minimax.chat/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "nous" => {
            let url = base_url
                .unwrap_or_else(|| "https://api.nousresearch.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "copilot" | "copilot-acp" => {
            let url = base_url
                .unwrap_or_else(|| "https://api.github.com/copilot".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let mut g = GenericProvider::new(url, &api_key, model_name);
            if let Some(pool) = credential_pool {
                g = g.with_credential_pool(pool.clone());
            }
            Arc::new(g)
        }
    };
    Ok(provider_obj)
}

/// Pick the [`CredentialPool`] reference for a route, honoring the
/// `credential_pool_fallback` flag. Returns `None` if the route doesn't
/// specify a pool and `credential_pool_fallback` is false (or there's no
/// primary pool to fall back to).
pub(crate) fn credential_pool_for_route<'a>(
    primary_credential_pool: Option<&'a Arc<CredentialPool>>,
    rt: &'a TurnRuntimeRoute,
) -> Option<&'a Arc<CredentialPool>> {
    if rt.credential_pool_fallback {
        rt.credential_pool
            .as_ref()
            .or(primary_credential_pool)
    } else {
        rt.credential_pool.as_ref()
    }
}
