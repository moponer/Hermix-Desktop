//! Shared agent construction functions.
//!
//! These helpers build an [`AgentConfig`], LLM provider, and tool-registry
//! bridge from a [`GatewayConfig`] + model string.  Both the CLI (`app.rs`)
//! and the Dashboard (`lib.rs`) import from here so the logic lives in one
//! place.

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use serde_json::Value;

use hermix_config::GatewayConfig;
use hermix_core::{AgentError, LlmProvider, ToolHandler};
use hermix_tools::{ToolCallContext, ToolRegistry};
use hermix_tools::tools::memory::MemoryHandler;
use tokio::runtime::Handle as TokioHandle;

use crate::agent_loop::{
    CheapModelRouteConfig, RuntimeProviderConfig, SmartModelRoutingConfig,
    ToolRegistry as AgentToolRegistry,
};
use crate::provider::{AnthropicProvider, GenericProvider, OpenRouterProvider};
use crate::AgentConfig;

// ---------------------------------------------------------------------------
// parse_model_string
// ---------------------------------------------------------------------------

/// Split a `"provider:model"` string into `(provider, model)`.
/// If no colon is present the provider defaults to `"openai"`.
pub fn parse_model_string(model: &str) -> (&str, &str) {
    model.split_once(':').unwrap_or(("openai", model))
}

// ---------------------------------------------------------------------------
// build_agent_config
// ---------------------------------------------------------------------------

/// Build an [`AgentConfig`] from a [`GatewayConfig`] and a model string.
/// `platform` lets callers tag the config with their origin (`"cli"`,
/// `"http"`, `"gateway"`, …).  When `None` the field is left unset.
pub fn build_agent_config(
    config: &GatewayConfig,
    model: &str,
    platform: Option<&str>,
) -> AgentConfig {
    // (startup guard): `BudgetConfig` is the most common target of
    // user typos (off-by-thousand off-by-million). Validate it eagerly
    // here so a broken `~/.hermix/config.yaml` is visible in the first
    // log line of every command. The non-fatal variant keeps the
    // function signature stable for existing call sites — strict
    // callers should call `crate::agent_config::validate(&cfg)?`
    // directly.
    config.budget.validate_or_warn();
    let provider_from_model = model.split_once(':').map(|(p, _)| p.to_string());
    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        hermix_home: config.home_dir.clone(),
        provider: provider_from_model,
        stream: config.streaming.enabled,
        platform: platform.map(|s| s.to_string()),
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        base_url: cfg.base_url.clone(),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        smart_model_routing: SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        //P6 direction 3 : copy the persisted permission
        // profile from GatewayConfig into AgentConfig so the runtime
        // gate sees the same scope the user authorised via the
        // dashboard "Always Allow" button. Without this copy, the
        // profile lives only in `config.yaml` and is silently
        // ignored at session start.
        permission_profile: config.permission_profile.clone(),
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        //explicit master off-switch from
        // `~/.hermix/config.yaml` `agent.auto_skill_extraction`.
        // The user can opt out without touching the broader
        // background_review_enabled flag. We also honor the
        // `skills.auto_extraction` mirror (set independently in
        // the YAML under `skills:`) so existing users who already
        // track skill toggles under that key get the same effect.
        auto_skill_extraction: config.agent.auto_skill_extraction && config.skills.auto_extraction,
        autonomous_mode: config.autonomous_mode,
        // Forward the full LlmProviderConfig map (including `context_window`
        // and `model_contexts` populated by `POST /api/custom-providers/{id}/fetch-models`)
        // so `ContextManager::for_model_with_providers` resolves the real
        // context window for `custom-{id}:{model}` instead of falling back
        // to `DEFAULT_FALLBACK_CONTEXT` (128k).
        llm_providers: config.llm_providers.clone(),
        // Forward compaction pressure threshold so ContextManager uses the
        // user-configured ratio instead of the hardcoded 0.50 default.
        context_pressure_ratio: config.compaction.pressure_threshold_percent,
        //forward the desktop Settings →
        // Advanced tab values into the live AgentConfig. Each field is
        // `Option<…>` on the hermix-config side, so a `None` means "user
        // didn't customise" and we keep the hardcoded default from
        // `SubAgentConfig::default()` — preserving full backward compat
        // for existing config.yaml files that lack the `advanced:` block.
        sub_agent: {
            let mut sa = AgentConfig::default().sub_agent;
            if let Some(v) = config.advanced.sub_agent_timeout_secs {
                sa.timeout_secs = v;
            }
            if let Some(v) = config.advanced.sub_agent_min_turns {
                sa.min_turns = v;
            }
            if let Some(v) = config.advanced.sub_agent_max_turns_cap {
                sa.max_turns_cap = v;
            }
            if let Some(v) = config.advanced.delegation_max_iterations {
                sa.delegation_max_iterations = v;
            }
            sa
        },
        ..AgentConfig::default()
    }
}

// ---------------------------------------------------------------------------
// bridge_tool_registry
// ---------------------------------------------------------------------------

/// Bridge a full [`hermix_tools::ToolRegistry`] into the minimal
/// [`AgentToolRegistry`](crate::agent_loop::ToolRegistry) consumed by
/// [`AgentLoop`](crate::AgentLoop).
///
/// When `allowed_tool_names` is `Some`, only the tools whose `schema.name`
/// appears in that set are bridged. This implements the per-message tool
/// whitelist driven by [`AgentOverrides::tools`].
///
/// `ctx` is captured by every bridge closure so the per-session context
/// (workspace, approval hooks, session id, etc.) is available at
/// dispatch time without process-global mutable state. Each
/// `send_message` call builds its own `ToolCallContext` and passes it
/// here, so two concurrent sessions never share context.
///
/// `memory_handler`, when `Some`, overrides the process-global memory
/// backend for the `"memory"` tool. This is the core of memory
/// isolation: a per-session / per-room [`MemoryHandler`] (backed by its
/// own [`FileMemoryBackend`] directory) is bridged in so the agent only
/// ever reads/writes its own scoped memory and never the global
/// `~/.hermix/memories`. When `None` the global handler is used (legacy
/// behavior, kept for tools that do not yet pass a scope).
pub fn bridge_tool_registry(
    tools: &ToolRegistry,
    allowed_tool_names: Option<&HashSet<String>>,
    ctx: Arc<ToolCallContext>,
    memory_handler: Option<Arc<MemoryHandler>>,
) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        if let Some(allow) = allowed_tool_names {
            if !allow.contains(&schema.name) {
                continue;
            }
        }
        let name = schema.name.clone();
        // Scoped memory: when a per-session/room handler is supplied, the
        // `"memory"` tool must NOT dispatch through the global registry
        // (which would hit the shared global FileMemoryBackend). Instead we
        // register a dedicated closure that drives the isolated handler.
        // The handler is async; the agent loop calls this closure
        // synchronously, so we block_on the current tokio handle — the same
        // adaption pattern already used by the tool executor bridge.
        if name == "memory" {
            if let Some(mh) = memory_handler.clone() {
                agent_registry.register(
                    name.clone(),
                    schema,
                    Arc::new(
                        move |params: Value| -> Result<String, hermix_core::ToolError> {
                            // `Fn` closures may be invoked many times, so we
                            // clone the `Arc` on each call rather than moving
                            // the captured one. The handler is async; the
                            // agent loop calls this synchronously, so we
                            // block_on the current tokio handle.
                            let mh = mh.clone();
                            TokioHandle::current().block_on(async move {
                                mh.execute(params).await
                            })
                        },
                    ),
                );
                continue;
            }
        }
        let tools_clone = tools.clone();
        let ctx_clone = ctx.clone();
        agent_registry.register(
            name.clone(),
            schema,
            Arc::new(
                move |params: Value| -> Result<String, hermix_core::ToolError> {
                    Ok(tools_clone.dispatch_with_context(&name, params, &ctx_clone))
                },
            ),
        );
    }
    agent_registry
}

// ---------------------------------------------------------------------------
// provider_api_key_from_env
// ---------------------------------------------------------------------------

/// Resolve an API key / token for a named LLM provider from well-known
/// environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    let var = env_var_name_for_provider(provider);
    if var == "<unknown provider>" {
        return None;
    }
    std::env::var(var).ok().filter(|s| !s.is_empty())
}

/// Canonical environment variable name for a given provider. Public so the
/// CLI / desktop settings panel can render the same hint.
pub fn env_var_name_for_provider(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "qwen" => "DASHSCOPE_API_KEY",
        "kimi" | "moonshot" => "MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "nous" => "NOUS_API_KEY",
        "copilot" => "GITHUB_COPILOT_TOKEN",
        // Settings-view providers that were missing from this map —
        // without them the /api/env-set key is invisible to
        // provider_api_key_from_env, causing "no API key" errors.
        "siliconflow" => "SILICONFLOW_API_KEY",
        "zhipu"       => "ZHIPU_API_KEY",
        "volcengine"  => "ARK_API_KEY",
        "dashscope"   => "DASHSCOPE_API_KEY",
        // 4 个新增国内厂商
        "baichuan"    => "BAICHUAN_API_KEY",
        "tencent"     => "HUNYUAN_API_KEY",
        "spark"       => "SPARK_API_KEY",
        "yi"          => "YI_API_KEY",
        _ => "<unknown provider>",
    }
}

// ---------------------------------------------------------------------------
// build_provider
// ---------------------------------------------------------------------------

/// Build an `Arc<dyn LlmProvider>` from a [`GatewayConfig`] and model string.
/// Returns `Err(AgentError::Config(...))` when no API key can be resolved for
/// the requested provider. **This is intentional** — silently falling back to
/// a no-op `StubProvider` was the root cause of the "stuck thinking" desktop
/// bug: the LLM stream returned an immediate error chunk the front-end never
/// surfaced, leaving the user staring at a "正在思考..." spinner forever.
/// Callers are expected to propagate the error to the user (the desktop
/// server turns it into a structured WS error event; the CLI exits with a
/// clear hint).
pub fn build_provider(
    config: &GatewayConfig,
    model: &str,
) -> Result<Arc<dyn LlmProvider>, AgentError> {
    let (provider_name, model_name) = parse_model_string(model);

    // AWS Bedrock / Converse + SigV4 is not implemented (Python v0.11.0 parity gap).
    // Do not silently route to [`GenericProvider`] against an OpenAI-compatible URL.
    if provider_name == "bedrock" {
        tracing::warn!(
            model = %model,
            "build_provider: AWS Bedrock is not implemented in hermes-agent-rust; see deploy/PARITY_MODULE_C.md (C1)"
        );
        return Ok(Arc::new(BedrockUnsupportedProvider {
            model: model.to_string(),
        }));
    }

    let provider_config = config.llm_providers.get(provider_name);

    let api_key = provider_config
        .and_then(|c| c.api_key.clone())
        .or_else(|| provider_api_key_from_env(provider_name));

    let api_key = match api_key {
        Some(k) if !k.trim().is_empty() => k,
        _ => {
            let env_var = env_var_name_for_provider(provider_name);
            let cfg_section = format!("[llm_providers.{provider_name}]");

            // Enumerate which providers *do* have keys, so the error message
            // tells the user "you configured X but the chat is using Y"
            // instead of just "no key for Y" (which made the user think the
            // system ignored their configuration).  We look at both
            // `llm_providers` (in-memory config) and the live process env.
            let mut configured: Vec<String> = Vec::new();
            for (name, cfg) in &config.llm_providers {
                if cfg.api_key.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
                    configured.push(name.clone());
                } else {
                    let v = env_var_name_for_provider(name);
                    if v != "<unknown provider>"
                        && std::env::var(&v)
                            .ok()
                            .filter(|s| !s.is_empty())
                            .is_some()
                    {
                        configured.push(name.clone());
                    }
                }
            }
            // Also probe a few common env vars the user might have set
            // outside the standard pipeline (e.g. raw shell export).
            for probe in [
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "MINIMAX_API_KEY",
                "DASHSCOPE_API_KEY",
                "ZHIPUAI_API_KEY",
                "VOLCENGINE_API_KEY",
                "SILICONFLOW_API_KEY",
                "OPENROUTER_API_KEY",
                "DEEPSEEK_API_KEY",
            ] {
                if std::env::var(probe)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .is_some()
                {
                    // The env var is set; map it back to a provider name so the
                    // message reads "you have openai, minimax" instead of
                    // "you have OPENAI_API_KEY, MINIMAX_API_KEY".
                    let friendly = match probe {
                        "OPENAI_API_KEY" => "openai",
                        "ANTHROPIC_API_KEY" => "anthropic",
                        "MINIMAX_API_KEY" => "minimax",
                        "DASHSCOPE_API_KEY" => "dashscope",
                        "ZHIPUAI_API_KEY" => "zhipu",
                        "VOLCENGINE_API_KEY" => "volcengine",
                        "SILICONFLOW_API_KEY" => "siliconflow",
                        "OPENROUTER_API_KEY" => "openrouter",
                        "DEEPSEEK_API_KEY" => "deepseek",
                        _ => continue,
                    };
                    if !configured.iter().any(|n| n == friendly) {
                        configured.push(friendly.to_string());
                    }
                }
            }
            configured.sort();
            configured.dedup();

            let mismatch_hint = if !configured.is_empty()
                && !configured.iter().any(|n| n == provider_name)
            {
                format!(
                    "\n  ⚠ Provider mismatch: this chat is requesting '{provider_name}' \
                     (model: '{model}'), but you have configured keys for: [{}]. \
                     Either configure '{provider_name}' in Settings → API Keys, or change \
                     the chat model to one of the configured providers.",
                    configured.join(", ")
                )
            } else if configured.is_empty() {
                "\n  No LLM provider has been configured yet. \
                 Open Settings → API Keys and add a key for any provider \
                 (OpenAI / Anthropic / MiniMax / Zhipu / DashScope / Volcengine / \
                 SiliconFlow / DeepSeek / OpenRouter / Moonshot / Kimi / Nous / Copilot), \
                 or add a Custom OpenAI-compatible provider."
                    .to_string()
            } else {
                String::new()
            };

            let msg = format!(
                "No API key configured for LLM provider '{provider_name}' (model: '{model}'). \
                 Set the {env_var} environment variable, or add an `api_key = \"...\"` line to \
                 the {cfg_section} section in your hermix config file. \
                 In the desktop app: open Settings → API Keys and add a key for '{provider_name}'.{mismatch_hint}"
            );
            tracing::error!(
                provider = provider_name,
                model = model,
                env_var = env_var,
                configured_providers = ?configured,
                "build_provider: missing API key; returning Config error (no silent fallback)"
            );
            return Err(AgentError::Config(msg));
        }
    };

    let base_url = provider_config.and_then(|c| c.base_url.clone());

    Ok(match provider_name {
        "openai" => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => {
            let mut p = OpenRouterProvider::new(&api_key).with_model(model_name);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            if let Some(cfg) = provider_config {
                if !cfg.provider_order.is_empty() {
                    p = p.with_provider_order(cfg.provider_order.clone());
                }
            }
            Arc::new(p)
        }
        "qwen" => {
            let url = base_url.unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string());
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
            let url = base_url.unwrap_or_else(|| "https://api.nousresearch.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "copilot" => {
            let url = base_url.unwrap_or_else(|| "https://api.github.com/copilot".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "deepseek" => {
            let url = base_url.unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        // Settings-view providers that use OpenAI-compatible APIs.
        // They all use GenericProvider with their own default base URL.
        "siliconflow" => {
            let url = base_url.unwrap_or_else(|| "https://api.siliconflow.cn/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "zhipu" => {
            let url = base_url.unwrap_or_else(|| "https://open.bigmodel.cn/api/paas/v4".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "volcengine" => {
            let url = base_url.unwrap_or_else(|| "https://ark.cn-beijing.volces.com/api/v3".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "dashscope" => {
            let url = base_url.unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        // 国内厂商（4 个新增）— 全部走 GenericProvider（OpenAI 兼容）
        "baichuan" => {
            let url = base_url.unwrap_or_else(|| "https://api.baichuan-ai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "tencent" => {
            // 腾讯混元（hunyuan）走自己的 v1 端点
            let url = base_url.unwrap_or_else(|| "https://hunyuan.tencent.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "spark" => {
            // 讯飞星火 v3.5 — OpenAI 兼容 chat 端点
            let url = base_url.unwrap_or_else(|| {
                "https://spark-api-open.xf-yun.com/v3.5/chat".to_string()
            });
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        "yi" => {
            // 零一万物（Yi 系列）OpenAI 兼容
            let url = base_url.unwrap_or_else(|| "https://api.lingyiwanwu.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
        _ => {
            // Check for a custom provider configured via env vars
            // (Settings → 自定义).  The naming convention is:
            // CUSTOM_{ID}_API_KEY   — the API key
            // CUSTOM_{ID}_BASE_URL  — the base URL
            // The model string is `custom-{id}:{model-name}` so the
            // provider_name here will be `custom-{id}`.
            let custom_provider_key = provider_name
                .strip_prefix("custom-")
                .map(|id| id.replace('-', "_").to_uppercase());
            let custom_api_key = custom_provider_key.as_ref().and_then(|id| {
                std::env::var(format!("CUSTOM_{id}_API_KEY")).ok()
                    .filter(|k| !k.is_empty())
            });
            let custom_base_url = custom_provider_key.as_ref().and_then(|id| {
                std::env::var(format!("CUSTOM_{id}_BASE_URL")).ok()
                    .filter(|u| !u.is_empty())
            });
            let (effective_key, effective_url) = if let Some(k) = custom_api_key {
                (k, custom_base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()))
            } else {
                // Fall back to the generic path (env var or default)
                (api_key.clone(), base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1".to_string()))
            };
            Arc::new(GenericProvider::new(effective_url, &effective_key, model_name))
        }
    })
}

// ---------------------------------------------------------------------------
// BedrockUnsupportedProvider — explicit parity gap (module C1)
// ---------------------------------------------------------------------------

/// Placeholder when the model string uses `bedrock:…` — Converse/SigV4 not wired.
pub struct BedrockUnsupportedProvider {
    pub model: String,
}

#[async_trait::async_trait]
impl LlmProvider for BedrockUnsupportedProvider {
    async fn chat_completion(
        &self,
        _messages: &[hermix_core::Message],
        _tools: &[hermix_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<hermix_core::LlmResponse, AgentError> {
        Err(AgentError::LlmApi(format!(
            "AWS Bedrock is not implemented in hermes-agent-rust (Python parity gap). \
             Model '{}'. See deploy/PARITY_MODULE_C.md (C1).",
            self.model
        )))
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermix_core::Message],
        _tools: &[hermix_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
        futures::stream::once(async move {
            Err(AgentError::LlmApi(
                "AWS Bedrock streaming is not implemented in hermes-agent-rust.".to_string(),
            ))
        })
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// (StubProvider removed — build_provider now returns AgentError::Config
// instead of silently falling back; see the function-level doc on
// build_provider for rationale.)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod build_provider_tests {
    use super::*;

    #[test]
    fn bedrock_provider_is_explicit_gap() {
        let cfg = GatewayConfig::default();
        // Make sure no env key is set so the only path that returns a
        // provider is the explicit Bedrock branch.
        for k in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "DEEPSEEK_API_KEY",
            "DASHSCOPE_API_KEY",
            "MOONSHOT_API_KEY",
            "MINIMAX_API_KEY",
            "NOUS_API_KEY",
            "GITHUB_COPILOT_TOKEN",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
        let p = build_provider(&cfg, "bedrock:us.anthropic.claude-3-5-sonnet-20241022-v2:0")
            .expect("bedrock should resolve to the explicit-gap provider");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(async { p.chat_completion(&[], &[], None, None, None, None).await })
            .expect_err("bedrock should error");
        let s = err.to_string();
        assert!(
            s.contains("Bedrock") || s.contains("bedrock"),
            "unexpected: {s}"
        );
    }

    #[test]
    fn missing_api_key_returns_config_error_no_silent_fallback() {
        // Make sure no env key is set so the resolution is forced
        // through the "no key" path.
        for k in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "DEEPSEEK_API_KEY",
            "DASHSCOPE_API_KEY",
            "MOONSHOT_API_KEY",
            "MINIMAX_API_KEY",
            "NOUS_API_KEY",
            "GITHUB_COPILOT_TOKEN",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
        let cfg = GatewayConfig::default();
        let err = match build_provider(&cfg, "openai:gpt-4o") {
            Ok(_) => panic!("must error when no API key is configured"),
            Err(e) => e,
        };
        match err {
            AgentError::Config(msg) => {
                assert!(msg.contains("openai"), "msg must name provider: {msg}");
                assert!(msg.contains("OPENAI_API_KEY"), "msg must name env var: {msg}");
                assert!(
                    !msg.contains("StubProvider"),
                    "no silent fallback: msg must not mention StubProvider: {msg}"
                );
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn config_provided_api_key_is_accepted() {
        // Make sure no env key bleeds in; only the config-supplied
        // key should make the build succeed.
        for k in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "DEEPSEEK_API_KEY",
            "DASHSCOPE_API_KEY",
            "MOONSHOT_API_KEY",
            "MINIMAX_API_KEY",
            "NOUS_API_KEY",
            "GITHUB_COPILOT_TOKEN",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
        let mut cfg = GatewayConfig::default();
        let mut openai_cfg =
            hermix_config::LlmProviderConfig::default();
        openai_cfg.api_key = Some("sk-test-from-config".to_string());
        cfg.llm_providers
            .insert("openai".to_string(), openai_cfg);
        let p = build_provider(&cfg, "openai:gpt-4o")
            .expect("config-supplied key should resolve");
        // We don't actually call the provider here (would need a mock
        // server); the existence of the build success is the
        // assertion.
        let _ = p;
    }
}
