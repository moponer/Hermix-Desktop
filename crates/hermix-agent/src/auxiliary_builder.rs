//! Wires the abstract [`AuxiliaryClient`] (defined in `hermes-intelligence`)
//! to concrete [`LlmProvider`] implementations from this crate.
//!
//! The intelligence crate cannot depend on `hermes-agent` (would create a
//! cycle), so the binary layer owns the provider construction. This module
//! reads standard environment variables and assembles the auto-detect chain
//! that mirrors the Python `_get_provider_chain` ordering.

use std::sync::Arc;

use hermix_config::GatewayConfig;
use hermix_core::LlmProvider;
use hermix_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryConfig, AuxiliarySource, ProviderCandidate,
};

use crate::provider::{AnthropicProvider, GenericProvider, OpenRouterProvider};

/// Default auxiliary models per source. Mirrors the Python
/// `_API_KEY_PROVIDER_AUX_MODELS` table — chosen to be cheap and fast.
mod default_models {
    pub const OPENROUTER: &str = "google/gemini-3-flash-preview";
    pub const ANTHROPIC: &str = "claude-haiku-4-5-20251001";
    pub const OPENAI: &str = "gpt-4o-mini";
    pub const ZAI: &str = "glm-4.5-flash";
    pub const KIMI: &str = "kimi-k2-turbo-preview";
    pub const MINIMAX: &str = "MiniMax-M3";
    pub const GEMINI: &str = "gemini-3-flash-preview";
}

/// Returned by [`build_default_auxiliary_client`] alongside the client so
/// callers can introspect what was wired (e.g. for `hermes status`).
#[derive(Debug, Clone)]
pub struct AuxiliaryWiringSummary {
    pub registered: Vec<String>,
    pub skipped: Vec<String>,
}

/// Build an [`AuxiliaryClient`] from environment variables.
///
/// Resolution rules:
///
/// * `OPENROUTER_API_KEY` → registers an `openrouter` candidate
/// * `ANTHROPIC_API_KEY` → registers an `anthropic` candidate
/// * `OPENAI_API_KEY` (+ optional `OPENAI_BASE_URL`) → registers a `custom`
///   candidate
/// * `ZAI_API_KEY`, `KIMI_API_KEY`, `MINIMAX_API_KEY`, `GEMINI_API_KEY` →
///   register direct-key candidates (OpenAI-compatible base URLs)
///
/// Order matches Python: OpenRouter > Custom > Anthropic > direct keys.
///
/// **Deprecated**: prefer [`build_auxiliary_client_with_config`] which also
/// reads API keys from `config.yaml`'s `[llm_providers]` section. This
/// env-only constructor is kept for backward compatibility (tests, CLI).
pub fn build_default_auxiliary_client(
    config: AuxiliaryConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client_inner(config, None)
}

/// Build an [`AuxiliaryClient`] from both `config.yaml` and environment variables.
///
/// For each provider, the key is resolved as:
///   1. `config.llm_providers[provider].api_key` (from config.yaml)
///   2. Environment variable (e.g. `MINIMAX_API_KEY`)
///
/// This mirrors the main provider's resolution logic in `build_provider`,
/// ensuring the compression auxiliary client uses the same API key as the
/// main conversation. Without this, a user who configured their key only in
/// config.yaml (not as an env var) would see 401 errors from the compression
/// health check even though normal conversations work fine.
pub fn build_auxiliary_client_with_config(
    config: AuxiliaryConfig,
    gateway: &GatewayConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client_inner(config, Some(gateway))
}

/// Resolve an API key for a provider: config.yaml first, env var second.
fn resolve_key(gateway: Option<&GatewayConfig>, provider_name: &str, env_var: &str) -> Option<String> {
    // 1. config.yaml
    if let Some(gw) = gateway {
        if let Some(pc) = gw.llm_providers.get(provider_name) {
            if let Some(ref k) = pc.api_key {
                if !k.trim().is_empty() {
                    return Some(k.trim().to_string());
                }
            }
        }
    }
    // 2. env var
    if let Ok(k) = std::env::var(env_var) {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    None
}

/// Resolve base URL: config.yaml first, env var second, fallback to default.
fn resolve_base_url(
    gateway: Option<&GatewayConfig>,
    provider_name: &str,
    env_var: &str,
    default: &str,
) -> String {
    if let Some(gw) = gateway {
        if let Some(pc) = gw.llm_providers.get(provider_name) {
            if let Some(ref u) = pc.base_url {
                if !u.trim().is_empty() {
                    return u.trim().to_string();
                }
            }
        }
    }
    std::env::var(env_var)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn build_auxiliary_client_inner(
    config: AuxiliaryConfig,
    gateway: Option<&GatewayConfig>,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    let mut summary = AuxiliaryWiringSummary {
        registered: Vec::new(),
        skipped: Vec::new(),
    };
    let mut builder = AuxiliaryClient::builder().config(config);

    // OpenRouter
    if let Some(key) = resolve_key(gateway, "openrouter", "OPENROUTER_API_KEY") {
        let provider: Arc<dyn LlmProvider> = Arc::new(
            OpenRouterProvider::new(&key)
                .with_model(default_models::OPENROUTER)
                .with_http_referer("https://hermes-agent.nousresearch.com")
                .with_x_title("Hermix"),
        );
        builder = builder.add_candidate(ProviderCandidate::new(
            AuxiliarySource::OpenRouter,
            default_models::OPENROUTER,
            provider,
        ));
        summary.registered.push("openrouter".into());
    } else {
        summary.skipped.push("openrouter (no key)".into());
    }

    // Custom OpenAI-compatible endpoint
    if let Some(key) = resolve_key(gateway, "openai", "OPENAI_API_KEY") {
        let base_url = resolve_base_url(gateway, "openai", "OPENAI_BASE_URL", "https://api.openai.com/v1");
        let model = std::env::var("OPENAI_AUXILIARY_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default_models::OPENAI.into());
        let provider: Arc<dyn LlmProvider> =
            Arc::new(GenericProvider::new(base_url, &key, model.clone()));
        builder = builder.add_candidate(ProviderCandidate::new(
            AuxiliarySource::Custom,
            model,
            provider,
        ));
        summary.registered.push("custom".into());
    } else {
        summary.skipped.push("custom (no key)".into());
    }

    // Anthropic
    if let Some(key) = resolve_key(gateway, "anthropic", "ANTHROPIC_API_KEY") {
        let provider: Arc<dyn LlmProvider> =
            Arc::new(AnthropicProvider::new(&key).with_model(default_models::ANTHROPIC));
        builder = builder.add_candidate(ProviderCandidate::new(
            AuxiliarySource::Anthropic,
            default_models::ANTHROPIC,
            provider,
        ));
        summary.registered.push("anthropic".into());
    } else {
        summary.skipped.push("anthropic (no key)".into());
    }

    // Direct-key providers (env-var only — config.yaml names may not match)
    register_direct_key(
        &mut builder,
        &mut summary,
        "ZAI_API_KEY",
        "zai",
        "https://api.z.ai/api/coding/paas/v4",
        default_models::ZAI,
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "KIMI_API_KEY",
        "kimi",
        "https://api.moonshot.ai/v1",
        default_models::KIMI,
    );

    // MiniMax — config.yaml first, env var second
    if let Some(key) = resolve_key(gateway, "minimax", "MINIMAX_API_KEY") {
        let base_url = resolve_base_url(gateway, "minimax", "MINIMAX_BASE_URL", "https://api.minimax.io/v1");
        let provider: Arc<dyn LlmProvider> = Arc::new(GenericProvider::new(
            base_url,
            &key,
            default_models::MINIMAX,
        ));
        builder = builder.add_candidate(ProviderCandidate::new(
            AuxiliarySource::DirectKey("minimax".to_string()),
            default_models::MINIMAX,
            provider,
        ));
        summary.registered.push("minimax".into());
    } else {
        summary.skipped.push("minimax (no key)".into());
    }

    register_direct_key(
        &mut builder,
        &mut summary,
        "GEMINI_API_KEY",
        "gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        default_models::GEMINI,
    );

    let client = builder.build();
    (client, summary)
}

fn register_direct_key(
    builder: &mut hermix_intelligence::auxiliary::AuxiliaryClientBuilder,
    summary: &mut AuxiliaryWiringSummary,
    env_var: &str,
    label: &str,
    base_url: &str,
    default_model: &str,
) {
    let owned;
    let key = match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => {
            owned = v;
            owned.trim()
        }
        _ => {
            summary.skipped.push(format!("{label} (no key)"));
            return;
        }
    };
    let provider: Arc<dyn LlmProvider> =
        Arc::new(GenericProvider::new(base_url, key, default_model));
    let temp_builder = std::mem::take(builder);
    *builder = temp_builder.add_candidate(ProviderCandidate::new(
        AuxiliarySource::DirectKey(label.to_string()),
        default_model,
        provider,
    ));
    summary.registered.push(label.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_AUXILIARY_MODEL",
        "ANTHROPIC_API_KEY",
        "ZAI_API_KEY",
        "KIMI_API_KEY",
        "MINIMAX_API_KEY",
        "GEMINI_API_KEY",
    ];

    /// Save current env, then clear; restored when the guard drops.
    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let mut previous = Vec::new();
            for k in KEYS {
                previous.push((*k, std::env::var(k).ok()));
                std::env::remove_var(k);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.previous.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Run all env-mutating cases serially in a single test to avoid races
    /// with parallel cargo test workers (env is process-global).
    #[test]
    fn build_default_auxiliary_client_scenarios() {
        let _g = EnvGuard::clear();

        // Scenario 1: no keys → empty chain.
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 0, "empty env produced non-empty chain");
            assert!(summary.registered.is_empty());
            assert!(!summary.skipped.is_empty());
        }

        // Scenario 2: only OpenRouter.
        std::env::set_var("OPENROUTER_API_KEY", "sk-test");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 1);
            assert_eq!(client.chain_labels(), vec!["openrouter"]);
            assert_eq!(summary.registered, vec!["openrouter"]);
        }
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 3: full chain, deterministic order.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or");
        std::env::set_var("OPENAI_API_KEY", "sk-oa");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-an");
        std::env::set_var("ZAI_API_KEY", "z");
        {
            let (client, _) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(
                client.chain_labels(),
                vec!["openrouter", "custom", "anthropic", "zai"]
            );
        }
    }
}
