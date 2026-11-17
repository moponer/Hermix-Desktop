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
use std::time::Duration;

use tokio::time::sleep;

use hermix_core::{AgentError, LlmProvider, LlmResponse, ToolSchema};

use crate::agent_config::AgentConfig;
use crate::agent_helpers::{
    classify_error, jittered_backoff, parse_retry_after_ms, AgentCallbacks, ErrorClass,
};
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

/// H1 观测（docs/optimization-plan.md）：上次请求的 prefix 指纹。
/// hash 不变 = 前缀稳定（缓存友好）；变化即记 `cache_break`——这是唯一能
/// **证明**"没在破坏缓存"的手段（lynn 17 §4）。进程级即可：重启必然
/// 是新前缀，记 `first` 没有诊断价值。
static LAST_PREFIX: Mutex<Option<String>> = Mutex::new(None);

/// prefix 指纹：模型 + 工具面 + 全部 API 消息的 md5。任何一处变化都会
/// 改变指纹，与端点的 prompt-cache 前缀语义一致。
fn prefix_fingerprint(
    config: &AgentConfig,
    api_messages: &[hermix_core::Message],
    tool_schemas: &[ToolSchema],
) -> String {
    let mut material = config.model.clone();
    material.push('|');
    material.push_str(&serde_json::to_string(tool_schemas).unwrap_or_default());
    material.push('|');
    material.push_str(&serde_json::to_string(api_messages).unwrap_or_default());
    format!("{:x}", md5::compute(material.as_bytes()))
}

/// Build the routed or primary provider, retrying OAuth refresh once on
/// auth failures so an expired token doesn't burn retry slots.
async fn build_provider_for_call(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    llm_provider: &Arc<dyn LlmProvider>,
    route: Option<&TurnRuntimeRoute>,
    model: &str,
) -> (Arc<dyn LlmProvider>, String) {
    let (provider_name, model_name) = extract_provider_and_model(config, model);
    if let Some(rt) = route {
        let mode = rt.api_mode.as_ref().unwrap_or(&config.api_mode);
        let pool = credential_pool_for_route(primary_credential_pool, rt);
        let provider_str = rt.provider.as_deref().unwrap_or(provider_name.as_str());
        match build_runtime_provider(
            config, oauth,
            provider_str, model_name,
            rt.base_url.as_deref(), rt.api_key_env.as_deref(),
            None, Some(mode), pool,
        ) {
            Ok(p) => return (p, model_name.to_string()),
            Err(e) => {
                tracing::warn!(
                    reason = ?rt.routing_reason,
                    error = %e,
                    "Runtime route unavailable, falling back to primary"
                );
            }
        }
    }
    // Fallback to primary
    let (_, primary_model) = extract_provider_and_model(config, config.model.as_str());
    (llm_provider.clone(), primary_model.to_string())
}

/// Execute one LLM API call against the given provider/model.
async fn execute_llm_call(
    provider: &Arc<dyn LlmProvider>,
    api_messages: &[hermix_core::Message],
    tool_schemas: &[ToolSchema],
    config: &AgentConfig,
    model: &str,
    extra_body: Option<&serde_json::Value>,
) -> Result<LlmResponse, AgentError> {
    provider
        .chat_completion(
            api_messages,
            tool_schemas,
            config.max_tokens,
            config.temperature,
            Some(model),
            extra_body,
        )
        .await
}

/// Single retry-instrumented LLM call (non-streaming).
/// Retry discipline:
/// - Route build failures → fall back to primary runtime, do NOT consume retry slot
/// - Auth (401) failures → refresh OAuth token once, retry; still fatal if refresh fails
/// - RateLimit / Retryable → retry with backoff, up to max_retries
/// - After all retries exhausted → try fallback_model once if configured
/// Returns the successful [`LlmResponse`] or an [`AgentError::LlmApi`]
/// when all attempts (including fallback) are exhausted.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "call_llm_with_retry",
    level = "info",
    skip_all,
    fields(
        request_model = tracing::field::Empty,
        resolved_model = tracing::field::Empty,
        route_label = tracing::field::Empty,
        attempt_count = tracing::field::Empty,
        fallback_used = tracing::field::Empty,
        outcome = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    )
)]
pub(crate) async fn call_llm_with_retry(
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

    if let Some(rt) = route {
        if let Some(ref label) = rt.route_label {
            tracing::debug!(%label, model = %rt.model, ?rt.signature, "smart model route");
        }
        if rt.command.is_some() || !rt.args.is_empty() {
            tracing::debug!(command = ?rt.command, args = ?rt.args, "smart route process metadata");
        }
    }

    let api_messages = messages_for_api_call(config, ctx);

    // H1 观测：prefixHash 断点诊断。指纹变化 = 缓存断点，落
    // `cache_break { reason }`（first = 会话首条，changed = 中途被改写）。
    let prefix_hash = prefix_fingerprint(config, &api_messages, tool_schemas);
    {
        let mut last = LAST_PREFIX.lock().expect("prefix lock poisoned");
        let reason = match last.as_deref() {
            None => "first",
            Some(prev) if prev != prefix_hash => "changed",
            _ => "",
        };
        if !reason.is_empty() {
            tracing::info!(
                target: "hermix.agent.cache",
                prefix_hash = %prefix_hash,
                reason,
                "cache_break"
            );
        }
        *last = Some(prefix_hash);
    }

    let retry = &config.retry;
    let (effective_max_retries, effective_base_delay_ms) =
        (retry.max_retries, retry.base_delay_ms);

    // Build the provider once outside the retry loop. Route build
    // failures don't consume retry slots — they immediately fall
    // back to the primary runtime.
    let mode = route
        .and_then(|r| r.api_mode.as_ref())
        .unwrap_or(&config.api_mode);
    let extra_body = extra_body_for_api_mode(config, mode);
    let (mut provider, mut resolved_model) = build_provider_for_call(
        config, oauth, primary_credential_pool, llm_provider, route, model,
    )
    .await;

    // Track whether we've already tried an OAuth refresh during this call.
    let mut oauth_refreshed = false;
    // H1 观测（docs/optimization-plan.md）：429/额度类错误计数——
    // 第 1 次退避重试，第 2 次即切 fallback_model（不再等重试穷尽）。
    let mut quota_hits = 0u32;

    for attempt in 0..=effective_max_retries {
        interrupt.check_interrupt()?;
        let pre_api_ctx = serde_json::json!({
            "attempt": attempt,
            "model": model,
            "stream": false,
            "route_label": route.and_then(|r| r.route_label.as_deref()),
        });
        let _ = invoke_hook(plugin_manager, HookType::PreApiRequest, &pre_api_ctx);

        // O1.2: race the LLM call against the interrupt signal. When the
        // user presses Ctrl+C (or the front-end sends a stop message),
        // `wait_for_interrupt()` resolves immediately — no need to wait
        // for the HTTP timeout or the next polling window.
        let result = tokio::select! {
            r = execute_llm_call(&provider, &api_messages, tool_schemas, config, &resolved_model, extra_body.as_ref()) => {
                r
            }
            _ = interrupt.wait_for_interrupt() => {
                Err(AgentError::Interrupted { message: interrupt.take_redirect_message() })
            }
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
            Ok(response) => {
                // H1 观测：逐轮增量告警——delta = prompt − cache_read，
                // 超阈值记 cache_delta_spike（lynn 17 §5）。
                if let Some(u) = &response.usage {
                    let delta = u.prompt_tokens.saturating_sub(u.cache_read_tokens);
                    if delta > 150 {
                        tracing::warn!(
                            target: "hermix.agent.cache",
                            delta,
                            prompt = u.prompt_tokens,
                            cache_read = u.cache_read_tokens,
                            "cache_delta_spike"
                        );
                    }
                }
                return Ok(response);
            }
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
                    ErrorClass::Auth => {
                        if !oauth_refreshed {
                            tracing::info!("Auth error — attempting OAuth token refresh");
                            oauth_refreshed = true;
                            oauth.refresh_if_needed(
                                &crate::runtime_provider::auth_tokens_path(config),
                                &config.runtime_providers,
                            )
                            .await;
                            // Rebuild provider with fresh token, retry without burning an attempt
                            (provider, resolved_model) = build_provider_for_call(
                                config, oauth, primary_credential_pool,
                                llm_provider, route, model,
                            )
                            .await;
                            continue;
                        }
                        return Err(AgentError::LlmApi(format!(
                            "Auth error persisted after token refresh: {err_str}"
                        )));
                    }
                    // Quota（429/额度类）：第 1 次退避后用主模型再试；
                    // 第 2 次即切 fallback_model（若配置且不同于主模型），
                    // 不等重试穷尽——额度耗尽后继续撞同一堵墙只会空转
                    //（lynn 01 复盘：429 后空转 100 分钟的教训）。
                    ErrorClass::Quota => {
                        quota_hits += 1;
                        tracing::warn!(
                            quota_hits,
                            model,
                            "LLM quota error (429/plan limit)"
                        );
                        if quota_hits >= 2 {
                            if let Some(ref fallback) = retry.fallback_model {
                                if request_model != fallback.as_str() {
                                    let (fallback_provider_name, fallback_model_only) =
                                        extract_provider_and_model(config, fallback);
                                    tracing::warn!(
                                        "Quota hit {}x on {}; switching to fallback: {}",
                                        quota_hits, model, fallback
                                    );
                                    let fallback_result = match build_runtime_provider(
                                        config, oauth,
                                        &fallback_provider_name, &fallback_model_only,
                                        None, None, None, None, None,
                                    ) {
                                        Ok(fb_provider) => {
                                            execute_llm_call(
                                                &fb_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                        Err(_) => {
                                            execute_llm_call(
                                                llm_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                    };
                                    return fallback_result
                                        .map_err(|e| AgentError::LlmApi(e.to_string()));
                                }
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        // 首次 quota：退避（尊重 Retry-After）后重试。
                        let delay = parse_retry_after_ms(&err_str)
                            .map(Duration::from_millis)
                            .unwrap_or_else(|| {
                                jittered_backoff(
                                    attempt,
                                    effective_base_delay_ms,
                                    retry.max_delay_ms,
                                )
                            });
                        tracing::info!("Quota backoff {}ms before retry", delay.as_millis());
                        sleep(delay).await;
                        continue;
                    }
                    ErrorClass::Fatal | ErrorClass::ContextOverflow => {
                        return Err(AgentError::LlmApi(err_str));
                    }
                    ErrorClass::RateLimit | ErrorClass::Retryable => {
                        // C1-3（docs/optimization-plan.md H4）：MiniMax 免费端点
                        // 的 529 整点高峰（provider 自述"1-5 分钟内恢复"）——
                        // 秒级退避对不上分钟级恢复窗口，重试穷尽必然报错
                        //（agent.log 实证：2026-11-16/17 两次 529 mid-stream）。
                        // overloaded 且配了 fallback：首次重试失败即切 fallback
                        //（换模型不等恢复）；无 fallback：退避拉长 ≥30s 对齐
                        // 恢复窗口，穷尽照旧报错。
                        let is_overloaded = {
                            let lower = err_str.to_lowercase();
                            lower.contains("529")
                                || lower.contains("overloaded")
                                || lower.contains("负载较高")
                        };
                        if is_overloaded && attempt >= 1 {
                            if let Some(ref fallback) = retry.fallback_model {
                                if request_model != fallback.as_str() {
                                    let (fallback_provider_name, fallback_model_only) =
                                        extract_provider_and_model(config, fallback);
                                    tracing::warn!(
                                        "Provider overloaded on {} (attempt {}). Switching to fallback: {}",
                                        model, attempt + 1, fallback
                                    );
                                    let fallback_result = match build_runtime_provider(
                                        config, oauth,
                                        &fallback_provider_name, &fallback_model_only,
                                        None, None, None, None, None,
                                    ) {
                                        Ok(fb_provider) => {
                                            execute_llm_call(
                                                &fb_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                        Err(_) => {
                                            execute_llm_call(
                                                llm_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                    };
                                    return fallback_result
                                        .map_err(|e| AgentError::LlmApi(e.to_string()));
                                }
                            }
                        }
                        if attempt >= effective_max_retries {
                            if let Some(ref fallback) = retry.fallback_model {
                                if request_model != fallback.as_str() {
                                    let (fallback_provider_name, fallback_model_only) =
                                        extract_provider_and_model(config, fallback);
                                    tracing::info!(
                                        "All retries exhausted on {}. Trying fallback: {}",
                                        model, fallback
                                    );
                                    // Build a provider for the fallback model —
                                    // llm_provider may not support it (e.g.
                                    // Anthropic provider can't call openai:gpt-4o-mini).
                                    let fallback_result = match build_runtime_provider(
                                        config, oauth,
                                        &fallback_provider_name, &fallback_model_only,
                                        None, None, None, None, None,
                                    ) {
                                        Ok(fb_provider) => {
                                            execute_llm_call(
                                                &fb_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                        Err(_) => {
                                            // Can't build fallback; try llm_provider as last resort
                                            execute_llm_call(
                                                llm_provider,
                                                &api_messages, tool_schemas, config,
                                                &fallback_model_only,
                                                extra_body_for_api_mode(config, &config.api_mode).as_ref(),
                                            ).await
                                        }
                                    };
                                    return fallback_result
                                        .map_err(|e| AgentError::LlmApi(e.to_string()));
                                }
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        // Provider-suggested wait (Retry-After header echoed
                        // into the error, or prose) wins over local backoff —
                        // the provider knows its own load. Absent/over-cap →
                        // local exponential backoff as before.
                        let delay = parse_retry_after_ms(&err_str)
                            .map(Duration::from_millis)
                            .unwrap_or_else(|| {
                                let base = jittered_backoff(
                                    attempt,
                                    effective_base_delay_ms,
                                    retry.max_delay_ms,
                                );
                                // C1-3：overloaded 恢复窗口是分钟级（provider
                                // 自述 1-5 分钟），秒级退避不够——拉长 ≥30s。
                                if is_overloaded {
                                    base.max(Duration::from_secs(30))
                                } else {
                                    base
                                }
                            });
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
        // agent_helpers::jittered_backoff(0, 1000, 30000):
        // exp=1000, capped=1000, jitter=250
        // delay in [875, 1125)
        let d = jittered_backoff(0, 1000, 30000);
        assert!(d.as_millis() >= 850);
        assert!(d.as_millis() <= 1150);
    }

    #[test]
    fn retry_after_header_seconds_is_honoured() {
        assert_eq!(
            parse_retry_after_ms("429 Too Many Requests (retry-after: 12)"),
            Some(12_000)
        );
    }

    #[test]
    fn retry_after_prose_is_honoured() {
        assert_eq!(
            parse_retry_after_ms("rate limited — please try again in 30 seconds"),
            Some(30_000)
        );
        assert_eq!(
            parse_retry_after_ms("provider overloaded: retry after 2 minutes"),
            Some(120_000)
        );
    }

    #[test]
    fn retry_after_caps_at_five_minutes_and_ignores_noise() {
        // 10 minutes over cap → ignored entirely (treated as absent).
        assert_eq!(
            parse_retry_after_ms("quota window: retry after 10 minutes"),
            None
        );
        // Unrelated durations don't hijack the backoff.
        assert_eq!(parse_retry_after_ms("request timed out after 30 seconds"), None);
    }
}
