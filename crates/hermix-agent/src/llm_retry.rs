//! LLM API call with retry loop (previously in agent_loop.rs).
//!
//! Houses `call_llm_with_retry` — the inner retry loop that:
//! 1. Loops up to `retry.max_retries + 1` (first attempt + retries)
//! 2. Forks on routed vs direct provider
//! 3. Classifies errors (Auth/Fatal/ContextOverflow vs RateLimit/Retryable)
//! 4. Backs off with jittered sleep
//! 5. Tries the fallback model once when all retries are exhausted
//!
//! All sub-calls (invoke_hook, emit_status, extract_provider_and_model,
//! build_runtime_provider, etc.) are free fns from their sibling
//! modules — no `AgentLoop` dependency.

use std::sync::{Arc, Mutex};

use tokio::time::sleep;

use hermix_core::{AgentError, LlmProvider, LlmResponse, ToolSchema};

use crate::agent_config::AgentConfig;
use crate::agent_helpers::{classify_error, jittered_backoff, AgentCallbacks, ErrorClass};
use crate::agent_loop::TurnRuntimeRoute;
use crate::context::ContextManager;
use crate::credential_pool::CredentialPool;
use crate::hook_dispatch::invoke_hook;
use crate::interrupt::InterruptController;
use crate::message_helpers::{emit_status, extra_body_for_api_mode};
use crate::oauth_refresh::OAuthRefresh;
use crate::plugins::{HookType, PluginManager};
use crate::prompt_assembly::{extract_provider_and_model, messages_for_api_call};
use crate::runtime_provider::{build_runtime_provider, credential_pool_for_route};

/// Single retry-instrumented LLM call (non-streaming).
///
/// Returns the successful [`LlmResponse`] or an [`AgentError::LlmApi`]
/// when all attempts (including optional fallback) are exhausted.
#[allow(clippy::too_many_arguments)]
pub async fn call_llm_with_retry(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    callbacks: &AgentCallbacks,
    llm_provider: &Arc<dyn LlmProvider>,
    plugin_manager: Option<&Arc<Mutex<PluginManager>>>,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    interrupt: &InterruptController,
    ctx: &ContextManager,
    tool_schemas: &[ToolSchema],
    route: Option<&TurnRuntimeRoute>,
) -> Result<LlmResponse, AgentError> {
    let model = route
        .map(|r| r.model.as_str())
        .unwrap_or(config.model.as_str());
    let (_, request_model) = extract_provider_and_model(config, model);
    let (_, primary_request_model) =
        extract_provider_and_model(config, config.model.as_str());
    if let Some(rt) = route {
        if let Some(ref label) = rt.route_label {
            tracing::debug!(%label, model = %rt.model, ?rt.signature, "smart model route");
        }
        if rt.command.is_some() || !rt.args.is_empty() {
            tracing::debug!(command = ?rt.command, args = ?rt.args, "smart route process metadata");
        }
    }
    let api_messages = messages_for_api_call(config, ctx);
    let retry = &config.retry;
    let (effective_max_retries, effective_base_delay_ms) =
        (retry.max_retries, retry.base_delay_ms);
    let default_extra_body = extra_body_for_api_mode(config, &config.api_mode);

    for attempt in 0..=effective_max_retries {
        interrupt.check_interrupt()?;
        let pre_api_ctx = serde_json::json!({
            "attempt": attempt,
            "model": model,
            "stream": false,
            "route_label": route.and_then(|r| r.route_label.as_deref()),
        });
        let _ = invoke_hook(plugin_manager, HookType::PreApiRequest, &pre_api_ctx);
        let result = if let Some(rt) = route {
            let (provider_name, model_name) = extract_provider_and_model(config, model);
            let mode = rt.api_mode.as_ref().unwrap_or(&config.api_mode);
            let extra_body_for_call = extra_body_for_api_mode(config, mode);
            let pool = credential_pool_for_route(primary_credential_pool, rt);
            let routed_provider = build_runtime_provider(
                config,
                oauth,
                rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                model_name,
                rt.base_url.as_deref(),
                rt.api_key_env.as_deref(),
                None,
                Some(mode),
                pool,
            );
            match routed_provider {
                Ok(provider) => {
                    provider
                        .chat_completion(
                            &api_messages,
                            tool_schemas,
                            config.max_tokens,
                            config.temperature,
                            Some(request_model),
                            extra_body_for_call.as_ref(),
                        )
                        .await
                }
                Err(e) => {
                    tracing::warn!(
                        "Runtime route unavailable (reason={:?}), falling back to primary runtime: {}",
                        rt.routing_reason,
                        e
                    );
                    llm_provider
                        .chat_completion(
                            &api_messages,
                            tool_schemas,
                            config.max_tokens,
                            config.temperature,
                            Some(primary_request_model),
                            default_extra_body.as_ref(),
                        )
                        .await
                }
            }
        } else {
            llm_provider
                .chat_completion(
                    &api_messages,
                    tool_schemas,
                    config.max_tokens,
                    config.temperature,
                    Some(request_model),
                    default_extra_body.as_ref(),
                )
                .await
        };
        let post_api_ctx = match &result {
            Ok(response) => serde_json::json!({
                "attempt": attempt,
                "model": model,
                "stream": false,
                "ok": true,
                "finish_reason": response.finish_reason,
                "has_tool_calls": response
                    .message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty()),
            }),
            Err(err) => serde_json::json!({
                "attempt": attempt,
                "model": model,
                "stream": false,
                "ok": false,
                "error": err.to_string(),
            }),
        };
        let _ = invoke_hook(plugin_manager, HookType::PostApiRequest, &post_api_ctx);

        match result {
            Ok(response) => return Ok(response),
            Err(e) => {
                let err_str = e.to_string();
                let class = classify_error(&err_str);
                tracing::warn!(
                    attempt,
                    error_class = ?class,
                    "LLM API error: {}",
                    &err_str[..err_str.len().min(200)]
                );

                match class {
                    ErrorClass::Auth | ErrorClass::Fatal => {
                        return Err(AgentError::LlmApi(err_str));
                    }
                    ErrorClass::ContextOverflow => {
                        return Err(AgentError::LlmApi(err_str));
                    }
                    ErrorClass::RateLimit | ErrorClass::Retryable => {
                        if attempt >= effective_max_retries {
                            if let Some(ref fallback) = retry.fallback_model {
                                if request_model != fallback.as_str() {
                                    let (_, fallback_request_model) =
                                        extract_provider_and_model(config, fallback);
                                    tracing::info!(
                                        "All retries exhausted on {}. Trying fallback: {}",
                                        model,
                                        fallback
                                    );
                                    let fallback_result = llm_provider
                                        .chat_completion(
                                            &api_messages,
                                            tool_schemas,
                                            config.max_tokens,
                                            config.temperature,
                                            Some(fallback_request_model),
                                            default_extra_body.as_ref(),
                                        )
                                        .await;
                                    return fallback_result
                                        .map_err(|e| AgentError::LlmApi(e.to_string()));
                                }
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        let delay = jittered_backoff(
                            attempt,
                            effective_base_delay_ms,
                            retry.max_delay_ms,
                        );
                        tracing::info!(
                            "Retrying in {}ms (attempt {}/{})",
                            delay.as_millis(),
                            attempt + 1,
                            effective_max_retries
                        );
                        emit_status(
                            config,
                            callbacks,
                            "lifecycle",
                            &format!(
                                "LLM API retry in {}ms (attempt {}/{})",
                                delay.as_millis(),
                                attempt + 1,
                                effective_max_retries
                            ),
                        );
                        sleep(delay).await;
                    }
                }
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittered_backoff_smoke() {
        let d = jittered_backoff(0, 1000, 30000);
        assert!(d.as_millis() >= 500);
        assert!(d.as_millis() <= 1000);
    }
}
