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
/// Resolution rules:
/// * `OPENROUTER_API_KEY` → registers an `openrouter` candidate
/// * `ANTHROPIC_API_KEY` → registers an `anthropic` candidate
/// * `OPENAI_API_KEY` (+ optional `OPENAI_BASE_URL`) → registers a `custom`
/// candidate
/// * `ZAI_API_KEY`, `KIMI_API_KEY`, `MINIMAX_API_KEY`, `GEMINI_API_KEY` →
/// register direct-key candidates (OpenAI-compatible base URLs)
/// Order matches Python: OpenRouter > Custom > Anthropic > direct keys.
/// **Deprecated**: prefer [`build_auxiliary_client_with_config`] which also
/// reads API keys from `config.yaml`'s `[llm_providers]` section. This
/// env-only constructor is kept for backward compatibility (tests, CLI).
pub fn build_default_auxiliary_client(
    config: AuxiliaryConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client_inner(config, None, None)
}

/// Build an [`AuxiliaryClient`] from both `config.yaml` and environment variables.
/// For each provider, the key is resolved as:
/// 1. `config.llm_providers[provider].api_key` (from config.yaml)
/// 2. Environment variable (e.g. `MINIMAX_API_KEY`)
/// This mirrors the main provider's resolution logic in `build_provider`,
/// ensuring the compression auxiliary client uses the same API key as the
/// main conversation. Without this, a user who configured their key only in
/// config.yaml (not as an env var) would see 401 errors from the compression
/// health check even though normal conversations work fine.
///
/// `preferred_model` is the *session's active model* (e.g. the per-conversation
/// override `"custom-agnes2-0:agnes-2.5-flash"`). It is promoted to the front
/// of the aux chain so the compression summary — which sends the bare model id
/// to the chain — hits the correct provider first instead of a wrong one that
/// 400s on an "unknown model". This matters because the compression summary
/// model follows the user's *active* model, which may differ from
/// `config.yaml`'s global `model:`.
pub fn build_auxiliary_client_with_config(
    config: AuxiliaryConfig,
    gateway: &GatewayConfig,
    preferred_model: Option<&str>,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client_inner(config, Some(gateway), preferred_model)
}

/// Map a `provider:model` (or bare model) string to the aux-chain candidate
/// label that should be promoted for it. Mirrors the alias logic used when
/// promoting `config.yaml`'s global `model:`.
fn provider_label_for(model: &str) -> &str {
    match crate::agent_builder::parse_model_string(model).0 {
        "openai" => "custom",
        "moonshot" => "kimi",
        other => other,
    }
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
    preferred_model: Option<&str>,
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
        // Match `build_provider`'s kimi/moonshot default (`https://api.moonshot.cn/v1`).
        "https://api.moonshot.cn/v1",
        default_models::KIMI,
    );

    // MiniMax — config.yaml first, env var second
    if let Some(key) = resolve_key(gateway, "minimax", "MINIMAX_API_KEY") {
        // NOTE: must match `build_provider`'s minimax default in agent_builder.rs
        // (`https://api.minimax.chat/v1`). The `.io` host rejects keys issued for
        // the `.chat` console with 401, which is exactly the "chat works but the
        // compression health check fails with invalid api key" symptom.
        let base_url = resolve_base_url(gateway, "minimax", "MINIMAX_BASE_URL", "https://api.minimax.chat/v1");
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

    // 补全: config.yaml `llm_providers` 里固定名单未覆盖的 provider
    // (例如用户自建的 `custom-*` 动态 provider, 或第三方 OpenAI 兼容
    // 网关)。主对话用哪个模型, 压缩摘要就应能用同一个 provider ——
    // 否则用户把主模型配成 gpt/agnes/自定义网关后, 压缩仍绕回上面
    // 固定名单里的候选, 甚至因为链里根本没有该 provider 而直接失败
    // ("LLM summary generation failed")。这里把剩余 provider 全部加进
    // 候选链, 压缩摘要即可跟随用户当前主模型。
    if let Some(gw) = gateway {
        for (name, pc) in &gw.llm_providers {
            // 固定名单已在上方注册过, 跳过避免重复候选。
            if matches!(
                name.as_str(),
                "openrouter" | "openai" | "anthropic" | "minimax" | "gemini" | "kimi" | "zai"
            ) {
                continue;
            }
            let key = pc
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(key) = key else {
                summary.skipped.push(format!("{name} (no key)"));
                continue;
            };
            let base_url = pc
                .base_url
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            // provider.model 可能是逗号分隔的模型列表 (UI 一次写多个);
            // 取第一个非空项作为该候选的默认模型。
            let model = pc
                .model
                .as_deref()
                .and_then(|m| m.split(',').map(str::trim).find(|s| !s.is_empty()))
                .unwrap_or(default_models::OPENAI)
                .to_string();
            let provider: Arc<dyn LlmProvider> =
                Arc::new(GenericProvider::new(base_url, key, model.clone()));
            builder = builder.add_candidate(ProviderCandidate::new(
                AuxiliarySource::DirectKey(name.clone()),
                model,
                provider,
            ));
            summary.registered.push(name.clone());
        }
    }

    // 主 model 的 provider 提到链首: 压缩摘要默认跟随主对话模型
    // (config.yaml `model:`), 若主 provider 是链尾的 custom-* 等动态
    // provider, 前序固定候选带 key 时会拿主 model id 去试 → 400 →
    // 非重试错误短路 → 压缩失败 ("LLM summary generation failed")。
    // promote 把主 provider 的候选挪到最前, 首个尝试即正确 provider;
    // 主 provider 不在链中时 promote 是 no-op, 由后续候选兜底。
    if let Some(gw) = gateway {
        if let Some(model) = gw.model.as_deref() {
            let label = provider_label_for(model);
            builder = builder.promote(label);
        }
    }

    // 会话实际使用的模型 (session override, 形如
    // "custom-agnes2-0:agnes-2.5-flash") 优先提到链首: 压缩摘要跟随
    // *当前* 主模型, 而上面那段按 config.yaml 全局 `model:` 排序 ——
    // 当两者不一致时 (例如本会话切到了 agnes, 但全局 model 是别的
    // provider), 链首不是 agnes, 裸 model 名 "agnes-2.5-flash" 先发到
    // 错误 provider (minimax) 触发 400 "unknown model", 且 400 是
    // 非重试错误 → 不 fallback → 压缩每次必失败。把 session 实际
    // provider 提到最前 (在全局 promote 之后, 所以它最终在头部),
    // 裸 model 名先发到正确 provider 即成功。
    if let Some(model) = preferred_model {
        let label = provider_label_for(model);
        builder = builder.promote(label);
    }

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
    use hermix_config::GatewayConfig;
    use std::collections::HashMap;

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

    // env 是进程全局的: 所有改 env 的测试必须串行 (并行 worker 会互相
    // 污染)。EnvyGuard 持锁跨整个测试生命周期。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Save current env, then clear; restored when the guard drops.
    struct EnvGuard {
        _env_lock: std::sync::MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let _env_lock = ENV_LOCK.lock().unwrap();
            let mut previous = Vec::new();
            for k in KEYS {
                previous.push((*k, std::env::var(k).ok()));
                std::env::remove_var(k);
            }
            Self { _env_lock, previous }
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

    /// Scenario 4: 主 model 的 provider 在链尾时被 promote 到链首。
    /// 覆盖 custom-* 动态 provider 作为主模型时, 前序固定候选 (openai 等)
    /// 有 key 也不会抢先拿主 model id 去试的修复 (D 类回归)。
    #[test]
    fn config_gateway_model_promotes_matching_provider_to_front() {
        let _g = EnvGuard::clear();

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-openai".into()),
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
        );
        providers.insert(
            "custom-agnes".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-agnes".into()),
                base_url: Some("https://api.agnes.ai/v1".into()),
                ..Default::default()
            },
        );
        let mut gateway = GatewayConfig::default();
        gateway.model = Some("custom-agnes:agnes-2.5-flash".into());
        gateway.llm_providers = providers;

        let (client, _) =
            build_auxiliary_client_with_config(AuxiliaryConfig::default(), &gateway, None);
        let labels = client.chain_labels();
        assert_eq!(labels[0], "custom-agnes", "主 provider 必须提到链首, got {labels:?}");
    }

    /// Scenario 5: openai 主 model 的 label 别名映射 (链中 openai 候选 label 是 "custom")。
    #[test]
    fn config_gateway_model_openai_maps_to_custom_label() {
        let _g = EnvGuard::clear();

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-openai".into()),
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
        );
        providers.insert(
            "anthropic".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-anthropic".into()),
                ..Default::default()
            },
        );
        let mut gateway = GatewayConfig::default();
        gateway.model = Some("openai:gpt-4o".into());
        gateway.llm_providers = providers;

        let (client, _) =
            build_auxiliary_client_with_config(AuxiliaryConfig::default(), &gateway, None);
        let labels = client.chain_labels();
        assert_eq!(labels[0], "custom", "openai 主 model 应映射到 custom label, got {labels:?}");
    }

    /// Scenario 6: 主 provider 不在链中 (无 key) 时 promote 是 no-op, 链序不变。
    #[test]
    fn config_gateway_model_absent_provider_is_noop() {
        let _g = EnvGuard::clear();

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-openai".into()),
                ..Default::default()
            },
        );
        let mut gateway = GatewayConfig::default();
        // 主 model 的 provider "deepseek" 没有任何 key → 链里没有该候选。
        gateway.model = Some("deepseek:deepseek-chat".into());
        gateway.llm_providers = providers;

        let (client, _) =
            build_auxiliary_client_with_config(AuxiliaryConfig::default(), &gateway, None);
        assert_eq!(client.chain_labels(), vec!["custom"]);
    }
}
