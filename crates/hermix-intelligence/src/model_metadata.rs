//! Model metadata, context lengths, and token estimation utilities.
//!
//! Pure utility functions with no agent dependency. Used by context
//! compression and run_agent for pre-flight context checks.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use hermix_config::LlmProviderConfig;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Descending tiers for context length probing when the model is unknown.
pub const CONTEXT_PROBE_TIERS: &[u64] = &[128_000, 64_000, 32_000, 16_000, 8_000];

/// Default context length when no detection method succeeds.
pub const DEFAULT_FALLBACK_CONTEXT: u64 = 128_000;

/// Minimum context length for Hermes Agent tool-calling workflows.
pub const MINIMUM_CONTEXT_LENGTH: u64 = 64_000;

// ---------------------------------------------------------------------------
// ModelMetadataEntry
// ---------------------------------------------------------------------------

/// Metadata about a model from a provider catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadataEntry {
    pub context_length: u64,
    #[serde(default)]
    pub max_completion_tokens: Option<u64>,
    pub name: String,
    #[serde(default)]
    pub pricing: Option<HashMap<String, f64>>,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_streaming: bool,
}

// ---------------------------------------------------------------------------
// Static default context lengths (thin fallback)
// ---------------------------------------------------------------------------

/// Known model context lengths — broad family patterns only.
/// Provider-specific resolution should use live APIs first.
static DEFAULT_CONTEXT_LENGTHS: &[(&str, u64)] = &[
    // Claude 4.6 (1M context)
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-opus-4.6", 1_000_000),
    ("claude-sonnet-4.6", 1_000_000),
    // Older Claude
    ("claude", 200_000),
    // OpenAI
    ("gpt-4.1", 1_047_576),
    ("gpt-5", 128_000),
    ("gpt-4", 128_000),
    // Google
    ("gemini", 1_048_576),
    ("gemma-4-31b", 256_000),
    ("gemma-3", 131_072),
    ("gemma", 8_192),
    // DeepSeek
    ("deepseek", 128_000),
    // Meta
    ("llama", 131_072),
    // Qwen
    ("qwen3-coder-plus", 1_000_000),
    ("qwen3-coder", 262_144),
    ("qwen", 131_072),
    // MiniMax
    //
    // M3.1 系列把上下文扩到 1M（用户 2026-10-23 实测确认）；M2 及更早仍是
    // 204.8K。条目按 key 长度降序匹配，所以具体型号必须排在家族前——否则
    // `("minimax", 204_800)` 会先命中，M3.1 永远读成 200K（这正是之前的
    // 症状：模型自称 1M，项目按 205K 触发压缩）。
    ("minimax-m3.1", 1_000_000),
    ("minimax", 204_800),
    // GLM
    ("glm", 202_752),
    // xAI Grok
    ("grok-4-1-fast", 2_000_000),
    ("grok-4-fast", 2_000_000),
    ("grok-4.20", 2_000_000),
    ("grok-code-fast", 256_000),
    ("grok-4", 256_000),
    ("grok-3", 131_072),
    ("grok-2", 131_072),
    ("grok", 131_072),
    // Kimi
    ("kimi", 262_144),
    // Agnes (custom provider family — typically served via custom-agnes:*).
    // Default to 128_000 (matches DEFAULT_FALLBACK_CONTEXT) so the catalog
    // hit returns the same value as the fallback; per-model overrides in
    // LlmProviderConfig.model_contexts take precedence when configured.
    ("agnes", 128_000),
];

// ---------------------------------------------------------------------------
// Model info (enriched)
// ---------------------------------------------------------------------------

/// Comprehensive model information including capabilities and pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output_tokens: Option<u64>,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning: bool,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
}

/// Static database of well-known models.
pub fn known_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            name: "gpt-4o".into(),
            provider: "openai".into(),
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(2.50),
            output_cost_per_million: Some(10.00),
        },
        ModelInfo {
            name: "gpt-4o-mini".into(),
            provider: "openai".into(),
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.15),
            output_cost_per_million: Some(0.60),
        },
        ModelInfo {
            name: "gpt-4.1".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(2.00),
            output_cost_per_million: Some(8.00),
        },
        ModelInfo {
            name: "gpt-4.1-mini".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.40),
            output_cost_per_million: Some(1.60),
        },
        ModelInfo {
            name: "gpt-4.1-nano".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.10),
            output_cost_per_million: Some(0.40),
        },
        ModelInfo {
            name: "o3".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(10.00),
            output_cost_per_million: Some(40.00),
        },
        ModelInfo {
            name: "o3-mini".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(1.10),
            output_cost_per_million: Some(4.40),
        },
        ModelInfo {
            name: "claude-opus-4-6".into(),
            provider: "anthropic".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(128_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(15.00),
            output_cost_per_million: Some(75.00),
        },
        ModelInfo {
            name: "claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(64_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(3.00),
            output_cost_per_million: Some(15.00),
        },
        ModelInfo {
            name: "claude-sonnet-4".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(3.00),
            output_cost_per_million: Some(15.00),
        },
        ModelInfo {
            name: "claude-3-5-haiku".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.80),
            output_cost_per_million: Some(4.00),
        },
        ModelInfo {
            name: "gemini-2.5-pro".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(1.25),
            output_cost_per_million: Some(10.00),
        },
        ModelInfo {
            name: "gemini-2.5-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(0.15),
            output_cost_per_million: Some(0.60),
        },
        ModelInfo {
            name: "gemini-2.0-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(8_192),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.10),
            output_cost_per_million: Some(0.40),
        },
        ModelInfo {
            name: "deepseek-chat".into(),
            provider: "deepseek".into(),
            context_window: 128_000,
            max_output_tokens: Some(8_192),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.14),
            output_cost_per_million: Some(0.28),
        },
        ModelInfo {
            name: "deepseek-reasoner".into(),
            provider: "deepseek".into(),
            context_window: 128_000,
            max_output_tokens: Some(8_192),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(0.55),
            output_cost_per_million: Some(2.19),
        },
        ModelInfo {
            name: "deepseek-v4-flash".into(),
            provider: "deepseek".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(384_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(0.14),
            output_cost_per_million: Some(0.28),
        },
        ModelInfo {
            name: "deepseek-v4-pro".into(),
            provider: "deepseek".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(384_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(1.74),
            output_cost_per_million: Some(3.48),
        },
    ]
}

// ---------------------------------------------------------------------------
// Lookup functions
// ---------------------------------------------------------------------------

/// Look up model info by name (substring match, longest wins).
pub fn get_model_info(model_name: &str) -> Option<ModelInfo> {
    let lower = model_name.to_lowercase();
    let models = known_models();
    let mut best: Option<&ModelInfo> = None;
    let mut best_len = 0;

    for info in &models {
        let key = info.name.to_lowercase();
        if lower.contains(&key) && key.len() > best_len {
            best = Some(info);
            best_len = key.len();
        }
    }

    best.cloned()
}

/// Check if a model supports vision.
pub fn supports_vision(model: &str) -> bool {
    get_model_info(model)
        .map(|m| m.supports_vision)
        .unwrap_or(false)
}

/// Check if a model supports tool/function calling.
pub fn supports_tools(model: &str) -> bool {
    get_model_info(model)
        .map(|m| m.supports_tools)
        .unwrap_or(true)
}

/// Get the maximum output tokens for a model.
pub fn max_output_tokens(model: &str) -> Option<u64> {
    get_model_info(model).and_then(|m| m.max_output_tokens)
}

/// Get the context window for a model using the hardcoded fallback table.
/// Resolution order:
/// 1. `known_models()` static catalog (covers Claude / OpenAI / Gemini / DeepSeek / etc.)
/// 2. `DEFAULT_CONTEXT_LENGTHS` fuzzy fallback table
/// 3. `DEFAULT_FALLBACK_CONTEXT` (128_000) for unknown / custom provider models
pub fn get_model_context_length(model: &str) -> u64 {
    let model_lower = model.to_lowercase();

    // Check known models first
    if let Some(info) = get_model_info(model) {
        return info.context_window;
    }

    // Hardcoded defaults (fuzzy match, longest key first)
    let mut entries: Vec<_> = DEFAULT_CONTEXT_LENGTHS.to_vec();
    entries.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
    for (key, length) in entries {
        if model_lower.contains(key) {
            return length;
        }
    }

    // Catalog miss — emit a single warn so operators can spot models that
    // need to be added to `known_models()` / `DEFAULT_CONTEXT_LENGTHS`.
    // Silently returning the fallback made the 200k-threshold bug invisible.
    tracing::warn!(
        target: "hermix_intelligence.model_metadata",
        model = %model,
        fallback_tokens = DEFAULT_FALLBACK_CONTEXT,
        "model not in static catalog; using DEFAULT_FALLBACK_CONTEXT"
    );
    DEFAULT_FALLBACK_CONTEXT
}

/// Get the context window for a model, honouring per-provider overrides.
/// Resolution order:
/// 1. If `providers` is supplied AND `model` matches `custom-{id}:{model_id}`
/// AND `LlmProviderConfig.model_contexts[model_id]` is set → that value.
/// 2. Same path with `LlmProviderConfig.context_window` (provider-wide default).
/// 3. Static catalog (`known_models()` + `DEFAULT_CONTEXT_LENGTHS`).
/// 4. `DEFAULT_FALLBACK_CONTEXT` (128_000) for unknown / custom provider models.
/// This lets the agent loop honour `custom-{provider_id}:{model}` overrides
/// set in `config.yaml` (or auto-populated by `POST /api/custom-providers/{id}/fetch-models`)
/// without forcing every custom provider model into the static catalog.
pub fn get_model_context_length_with(
    model: &str,
    providers: Option<&HashMap<String, LlmProviderConfig>>,
) -> u64 {
    get_model_context_length_with_source(model, providers).0
}

/// Which tier produced a resolved context window. Reported to the client so
/// the UI can label a number's origin ("探测" / "内置" / "兜底") instead of
/// re-deriving it — the front-end used to hand-mirror `DEFAULT_CONTEXT_LENGTHS`,
/// and the two copies drifted apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// `LlmProviderConfig::model_contexts[model_id]` — probed from the
    /// provider's `/v1/models` and persisted.
    Probed,
    /// `LlmProviderConfig::context_window` — provider-wide default.
    Provider,
    /// Static catalog (`known_models()` or `DEFAULT_CONTEXT_LENGTHS`).
    Catalog,
    /// Nothing matched — `DEFAULT_FALLBACK_CONTEXT`.
    Fallback,
}

impl ContextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::Provider => "provider",
            Self::Catalog => "catalog",
            Self::Fallback => "fallback",
        }
    }
}

/// Same resolution order as [`get_model_context_length_with`], but also
/// reports which tier won. This is the authoritative lookup — clients
/// should ask for it rather than replicating the catalog.
pub fn get_model_context_length_with_source(
    model: &str,
    providers: Option<&HashMap<String, LlmProviderConfig>>,
) -> (u64, ContextSource) {
    // 1. custom-{provider_id}:{model_id} → provider override
    if let Some(providers) = providers {
        if let Some((provider_id, model_id)) = model.split_once(':') {
            if let Some(provider) = providers.get(provider_id) {
                if let Some(&ctx) = provider.model_contexts.get(model_id) {
                    return (ctx, ContextSource::Probed);
                }
                if let Some(ctx) = provider.context_window {
                    return (ctx, ContextSource::Provider);
                }
            }
        } else {
            // 1b. 内置型号（`MiniMax-M3.1-Flash` 这类没有 provider 前缀）。
            //
            // 旧实现只在 `split_once(':')` 成功时才看 provider 覆盖，于是
            // **内置 provider 的上下文根本无法配置**：用户在 config.yaml 的
            // `llm_providers.minimax.model_contexts` 里写什么都被跳过，只能
            // 改代码里的静态表（2026-10-23 报告：M3.1 支持 1M，项目按
            // 205K 算）。
            //
            // 模型名跨 provider 唯一性足够，遍历匹配即可；大小写不敏感，
            // 因为前端 fetch-models 写入的是上游原样 id，用户手写时大小写
            // 常常对不上。
            for provider in providers.values() {
                if let Some((_, &ctx)) = provider
                    .model_contexts
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(model))
                {
                    return (ctx, ContextSource::Probed);
                }
            }
        }
    }

    // 2. **线上目录优先于静态表**（2026-10-23）。
    //
    // OpenRouter 的公开模型目录（免 key）带每个型号真实的 `context_length`，
    // 而下面的静态表是按家族前缀猜的——`("minimax", 204_800)` 在 M3.1
    // （1M）上照样生效，模型按 1M 干活、项目按 205K 触发压缩。
    // 目录快照是同步内存查（磁盘缓存 + 后台刷新），不阻塞这一轮。
    if let Some(limits) = crate::model_directory::lookup(model) {
        return (limits.context, ContextSource::Catalog);
    }

    // 3. Static catalog (inlined; do NOT recurse back into
    // `get_model_context_length` — that path doesn't accept overrides but
    // would still be functionally identical; inlining keeps the call chain
    // flat and avoids accidental infinite recursion.)
    let model_lower = model.to_lowercase();
    if let Some(info) = get_model_info(model) {
        return (info.context_window, ContextSource::Catalog);
    }
    let mut entries: Vec<_> = DEFAULT_CONTEXT_LENGTHS.to_vec();
    entries.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
    for (key, length) in entries {
        if model_lower.contains(key) {
            return (length, ContextSource::Catalog);
        }
    }

    // Catalog miss (after provider-override path) — same warn as the
    // without-overrides variant so operators can spot it regardless of
    // which API the agent loop called.
    tracing::warn!(
        target: "hermix_intelligence.model_metadata",
        model = %model,
        fallback_tokens = DEFAULT_FALLBACK_CONTEXT,
        "model not in catalog and no provider override; using DEFAULT_FALLBACK_CONTEXT"
    );
    (DEFAULT_FALLBACK_CONTEXT, ContextSource::Fallback)
}

/// Get the next lower probe tier for context length probing.
pub fn get_next_probe_tier(current: u64) -> Option<u64> {
    CONTEXT_PROBE_TIERS
        .iter()
        .copied()
        .find(|&tier| tier < current)
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Rough token estimate (~4 chars/token) for pre-flight checks.
/// Uses ceiling division so short texts never estimate as 0.
pub fn estimate_tokens_rough(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    (text.len() as u64).div_ceil(4)
}

/// Rough token estimate for a serialized message list.
pub fn estimate_messages_tokens_rough(messages: &[serde_json::Value]) -> u64 {
    let total_chars: usize = messages.iter().map(|m| m.to_string().len()).sum();
    (total_chars as u64).div_ceil(4)
}

/// Rough token estimate for a full request (messages + system + tools).
pub fn estimate_request_tokens_rough(
    messages: &[serde_json::Value],
    system_prompt: &str,
    tools: Option<&[serde_json::Value]>,
) -> u64 {
    let mut total_chars = system_prompt.len();
    total_chars += messages.iter().map(|m| m.to_string().len()).sum::<usize>();
    if let Some(tools) = tools {
        total_chars += tools.iter().map(|t| t.to_string().len()).sum::<usize>();
    }
    (total_chars as u64).div_ceil(4)
}

// ---------------------------------------------------------------------------
// Error parsing
// ---------------------------------------------------------------------------

/// Try to extract the actual context limit from an API error message.
pub fn parse_context_limit_from_error(error_msg: &str) -> Option<u64> {
    let lower = error_msg.to_lowercase();
    let patterns = &[
        r"(?:max(?:imum)?|limit)\s*(?:context\s*)?(?:length|size|window)?\s*(?:is|of|:)?\s*(\d{4,})",
        r"context\s*(?:length|size|window)\s*(?:is|of|:)?\s*(\d{4,})",
        r"(\d{4,})\s*(?:token)?\s*(?:context|limit)",
    ];

    for pattern in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&lower) {
                if let Some(m) = caps.get(1) {
                    if let Ok(limit) = m.as_str().parse::<u64>() {
                        if (1024..=10_000_000).contains(&limit) {
                            return Some(limit);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Detect "max_tokens too large" errors and return available output tokens.
pub fn parse_available_output_tokens_from_error(error_msg: &str) -> Option<u64> {
    let lower = error_msg.to_lowercase();
    if !lower.contains("max_tokens") || !lower.contains("available") {
        return None;
    }

    let patterns = &[
        r"available_tokens[:\s]+(\d+)",
        r"available\s+tokens[:\s]+(\d+)",
        r"=\s*(\d+)\s*$",
    ];

    for pattern in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&lower) {
                if let Some(m) = caps.get(1) {
                    if let Ok(tokens) = m.as_str().parse::<u64>() {
                        if tokens >= 1 {
                            return Some(tokens);
                        }
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Provider inference
// ---------------------------------------------------------------------------

/// Infer the provider name from a base URL.
pub fn infer_provider_from_url(base_url: &str) -> Option<&'static str> {
    let lower = base_url.to_lowercase();
    let mappings: &[(&str, &str)] = &[
        ("api.openai.com", "openai"),
        ("chatgpt.com", "openai"),
        ("api.anthropic.com", "anthropic"),
        ("api.z.ai", "zai"),
        ("api.moonshot.ai", "kimi-coding"),
        ("api.minimax", "minimax"),
        ("dashscope.aliyuncs.com", "alibaba"),
        ("openrouter.ai", "openrouter"),
        ("generativelanguage.googleapis.com", "gemini"),
        ("api.deepseek.com", "deepseek"),
        ("api.githubcopilot.com", "copilot"),
        ("api.x.ai", "xai"),
    ];

    for (pattern, provider) in mappings {
        if lower.contains(pattern) {
            return Some(provider);
        }
    }
    None
}

/// Check if a base URL points to a local machine.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let lower = base_url.to_lowercase();
    lower.contains("localhost")
        || lower.contains("127.0.0.1")
        || lower.contains("::1")
        || lower.contains("0.0.0.0")
        || lower.contains(".docker.internal")
        || lower.contains(".containers.internal")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_model_context_length() {
        assert_eq!(get_model_context_length("gpt-4o"), 128_000);
        assert_eq!(get_model_context_length("claude-opus-4-6"), 1_000_000);
        assert_eq!(get_model_context_length("gemini-2.0-flash"), 1_048_576);
        assert_eq!(
            get_model_context_length("unknown-model"),
            DEFAULT_FALLBACK_CONTEXT
        );
    }

    #[test]
    fn test_estimate_tokens_rough() {
        assert_eq!(estimate_tokens_rough(""), 0);
        assert_eq!(estimate_tokens_rough("hello world"), 3); // 11 chars -> 3
        assert_eq!(estimate_tokens_rough("hi"), 1); // 2 chars -> ceiling(5/4) = 1
    }

    #[test]
    fn test_get_model_info() {
        let info = get_model_info("gpt-4o").unwrap();
        assert_eq!(info.context_window, 128_000);
        assert!(info.supports_vision);
    }

    #[test]
    fn test_supports_vision() {
        assert!(supports_vision("gpt-4o"));
        assert!(supports_vision("claude-opus-4-6"));
        assert!(!supports_vision("deepseek-chat"));
    }

    #[test]
    fn test_get_next_probe_tier() {
        assert_eq!(get_next_probe_tier(128_000), Some(64_000));
        assert_eq!(get_next_probe_tier(64_000), Some(32_000));
        assert_eq!(get_next_probe_tier(8_000), None);
    }

    #[test]
    fn test_infer_provider() {
        assert_eq!(
            infer_provider_from_url("https://api.openai.com/v1"),
            Some("openai")
        );
        assert_eq!(
            infer_provider_from_url("https://api.anthropic.com"),
            Some("anthropic")
        );
        assert_eq!(infer_provider_from_url("http://localhost:8080"), None);
    }

    #[test]
    fn test_is_local_endpoint() {
        assert!(is_local_endpoint("http://localhost:8080"));
        assert!(is_local_endpoint("http://127.0.0.1:11434"));
        assert!(!is_local_endpoint("https://api.openai.com"));
    }

    #[test]
    fn test_get_model_context_length_with_per_model_override() {
        // `custom-agnes:agnes-2.0-flash` with a per-model override should
        // return the override value, not the catalog fallback.
        let mut providers = HashMap::new();
        let mut p = LlmProviderConfig::default();
        p.model_contexts
            .insert("agnes-2.0-flash".to_string(), 200_000);
        providers.insert("custom-agnes".to_string(), p);

        assert_eq!(
            get_model_context_length_with("custom-agnes:agnes-2.0-flash", Some(&providers)),
            200_000
        );
    }

    #[test]
    fn test_get_model_context_length_with_provider_wide_fallback() {
        // No per-model entry → fall back to provider.context_window.
        let mut providers = HashMap::new();
        let mut p = LlmProviderConfig::default();
        p.context_window = Some(256_000);
        providers.insert("custom-agnes".to_string(), p);

        assert_eq!(
            get_model_context_length_with("custom-agnes:unknown-model", Some(&providers)),
            256_000
        );
    }

    /// M3.1 的 1M 上下文必须命中具体型号条目，而不是被家族兜底吃掉。
    #[test]
    fn minimax_m31_resolves_to_one_million_not_the_family_default() {
        assert_eq!(
            get_model_context_length_with("MiniMax-M3.1-Flash", None),
            1_000_000,
            "M3.1 系列是 1M —— 家族兜底 204.8K 只适用于 M2 及更早"
        );
        // 更早的型号仍走家族兜底（不要误伤）。
        assert_eq!(
            get_model_context_length_with("MiniMax-M2", None),
            204_800,
            "M2 仍是 204.8K"
        );
    }

    /// 内置型号（无 provider 前缀）也要能读 `model_contexts` 覆盖。
    ///
    /// 2026-10-23 报告：用户按 1M 规格用 M3.1，项目按 205K 触发压缩，
    /// 而 config.yaml 里写 `model_contexts` 对内置 provider 不生效——
    /// 旧实现只在 `split_once(':')` 成功时才看覆盖。
    #[test]
    fn builtin_model_honours_a_model_context_override() {
        let mut providers = HashMap::new();
        let mut p = LlmProviderConfig::default();
        p.model_contexts
            .insert("MiniMax-M3.1-Flash".to_string(), 1_000_000);
        providers.insert("minimax".to_string(), p);

        // 大小写与配置里的 key 不同也要命中（前端写上游原样 id，用户手写
        // 常常大小写不一）。
        assert_eq!(
            get_model_context_length_with("minimax-m3.1-flash", Some(&providers)),
            1_000_000
        );
    }

    #[test]
    fn test_get_model_context_length_with_no_override_uses_catalog() {
        // Provider exists but no overrides set → fall through to catalog.
        let providers = HashMap::new();
        // custom-agnes:agnes-2.0-flash still hits the catalog via "agnes" key.
        assert_eq!(
            get_model_context_length_with("custom-agnes:agnes-2.0-flash", Some(&providers)),
            128_000
        );
    }

    #[test]
    fn test_get_model_context_length_with_per_model_wins_over_provider_wide() {
        // model_contexts beats context_window when both are set.
        let mut providers = HashMap::new();
        let mut p = LlmProviderConfig::default();
        p.context_window = Some(64_000);
        p.model_contexts.insert("big".to_string(), 1_000_000);
        providers.insert("custom-agnes".to_string(), p);

        assert_eq!(
            get_model_context_length_with("custom-agnes:big", Some(&providers)),
            1_000_000
        );
    }
}
