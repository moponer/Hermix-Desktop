//! Smart-model routing + cost-guard route construction.
//!
//! Phase 5 module reshape (2026-07-04). Mirrors the OAuthRefresh /
//! CostGuard sibling layout: each helper set gets its own file so
//! `agent_loop.rs` can shrink toward pure orchestration.
//!
//! All helpers here are pure functions over `&AgentLoop` + cheap
//! inputs (`&[Message]`, `&CheapModelRouteConfig`, model name string).
//! The cost-routing decision tree (`smart_model_routing::resolve_turn_route`)
//! lives in the `smart_model_routing` sibling and is the actual
//! strategy; this module is the thin wrapper that surfaces a
//! [`TurnRuntimeRoute`] (cost-guard / cheap-routed) to the turn loop.
//!
//! `latest_user_text` lives here because every site that needs "last
//! user message text" is a routing call (`resolve_smart_runtime_route`).
//!
//! `extract_provider_and_model` is NOT moved — it's used in 8 places
//! across `agent_loop.rs` (model rendering, fallback paths, smart
//! routing) and not specific to cost routing.

use std::sync::Arc;

use hermix_core::Message;

use crate::agent_config::AgentConfig;
use crate::agent_loop::{ApiMode, CheapModelRouteConfig, TurnRuntimeRoute};
use crate::credential_pool::CredentialPool;
use crate::oauth_refresh::OAuthRefresh;
use crate::smart_model_routing::{
    detect_api_mode_for_url, resolve_turn_route, PrimaryRuntime, ResolveTurnOutcome,
    ResolvedCheapRuntime,
};

/// Per-run route decision state. Holds the route chosen by the
/// cost-guard downgrade path (or any other path that wants to force
/// a specific route for the rest of the session).
///
/// Phase 5b (2026-07-04): extracted from `CostGuard::forced_runtime_route`
/// — that field belonged to routing, not spend accounting. The cost
/// guard now reads `route_state.forced_runtime_route` via `&RouteState`
/// in `evaluate_thresholds` and writes it via [`RouteState::install_downgrade`].
/// `CostGuard` only owns USD/usage/cost_warned; `RouteState` owns the
/// forced route decision.
#[derive(Debug, Default)]
pub(crate) struct RouteState {
    /// Forced cheap-model route for remaining turns. `None` when no
    /// downgrade has fired yet.
    pub(crate) forced_runtime_route: Option<TurnRuntimeRoute>,
}

impl RouteState {
    /// Install a forced route for the rest of the session. Called by
    /// the cost-guard warn path after the downgrade model resolves.
    /// `pub(crate)` because `TurnRuntimeRoute` is `pub(crate)`.
    pub(crate) fn install_downgrade(&mut self, route: TurnRuntimeRoute) {
        self.forced_runtime_route = Some(route);
    }
}

/// Find the most-recent user message text in a slice, or `None` if
/// there is no user message or the latest has no text content.
///
/// Used by [`resolve_smart_runtime_route`] to feed the keyword
/// router; `None` skips smart routing (the turn uses the primary
/// runtime as-is).
pub(crate) fn latest_user_text<'a>(messages: &'a [Message]) -> Option<&'a str> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Snapshot of the *primary* LLM runtime as configured, used by
/// [`resolve_smart_runtime_route`] as the baseline for "current
/// model" comparison. Mirrors what `run()` would default to if no
/// smart-routing decision fires.
pub(crate) fn primary_runtime_snapshot(
    config: &AgentConfig,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
) -> PrimaryRuntime {
    let provider = config
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let base_url = provider
        .as_ref()
        .and_then(|p| config.runtime_providers.get(p))
        .and_then(|c| {
            c.base_url
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let (command, args) =
        crate::runtime_provider::resolve_runtime_command_args(config, provider.as_deref());
    PrimaryRuntime {
        model: config.model.clone(),
        provider,
        base_url,
        api_mode: config.api_mode.clone(),
        command,
        args,
        credential_pool: primary_credential_pool.cloned(),
    }
}

/// Build a `TurnRuntimeRoute` that mirrors the primary runtime but
/// swaps in the supplied model — used by the cost guard to downgrade
/// the rest of the session to a cheaper model after the spending
/// threshold is tripped. Route label is `"cost_guard"` and the
/// credential pool fallback flag is `true` so the runtime pool is
/// used unless the route itself opts out.
pub(crate) fn turn_route_cost_guard(
    config: &AgentConfig,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    model: String,
) -> TurnRuntimeRoute {
    let pri = primary_runtime_snapshot(config, primary_credential_pool);
    let mut sig = pri.to_signature();
    sig.model = model.clone();
    TurnRuntimeRoute {
        model,
        provider: None,
        base_url: None,
        api_key_env: None,
        api_mode: None,
        command: None,
        args: Vec::new(),
        credential_pool: primary_credential_pool.cloned(),
        credential_pool_fallback: true,
        route_label: None,
        routing_reason: Some("cost_guard".to_string()),
        signature: sig,
    }
}

/// Attempt to materialize a [`ResolvedCheapRuntime`] from the cheap-
/// model route config + an explicit-key override. Returns `Err(())`
/// when the config is incomplete, the API key can't be resolved, or
/// (for `copilot-acp`) the `copilot` binary is missing. The smart
/// router treats `Err(())` as "cheap route unavailable" and falls
/// back to the primary runtime.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_build_cheap_runtime(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    cheap: &CheapModelRouteConfig,
    explicit_api_key: Option<String>,
) -> Result<ResolvedCheapRuntime, ()> {
    let provider_raw = cheap.provider.as_deref().map(str::trim).unwrap_or("");
    if provider_raw.is_empty() {
        return Err(());
    }
    let provider_lc = provider_raw.to_lowercase();
    let model_full = cheap.model.as_deref().map(str::trim).unwrap_or("");
    if model_full.is_empty() {
        return Err(());
    }
    let model_name = model_full
        .split_once(':')
        .map(|(_, m)| m)
        .unwrap_or(model_full);
    let base_url =
        crate::runtime_provider::resolve_runtime_base_url(config, &provider_lc, cheap.base_url.as_deref());
    let api_mode = base_url
        .as_deref()
        .and_then(detect_api_mode_for_url)
        .unwrap_or(ApiMode::ChatCompletions);

    let has_runtime_override = explicit_api_key
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .is_some()
        || cheap
            .base_url
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let pool_ref = if has_runtime_override {
        None
    } else {
        primary_credential_pool
    };

    crate::runtime_provider::build_runtime_provider(
        config,
        oauth,
        &provider_lc,
        model_name,
        cheap.base_url.as_deref(),
        cheap.api_key_env.as_deref(),
        explicit_api_key.as_deref(),
        Some(&api_mode),
        pool_ref,
    )
    .map_err(|_| ())?;

    let (command, args) =
        crate::runtime_provider::resolve_runtime_command_args(config, Some(&provider_lc));
    if provider_lc == "copilot-acp"
        && command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        && !base_url
            .as_deref()
            .map(|u| u.starts_with("acp+tcp://"))
            .unwrap_or(false)
    {
        return Err(());
    }
    if provider_lc == "copilot-acp"
        && !base_url
            .as_deref()
            .map(|u| u.starts_with("acp+tcp://"))
            .unwrap_or(false)
    {
        if let Some(cmd) = command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if which::which(cmd).is_err() {
                return Err(());
            }
        }
    }
    Ok(ResolvedCheapRuntime {
        model: model_full.to_string(),
        provider: provider_lc,
        base_url,
        api_mode,
        command,
        args,
        credential_pool: if has_runtime_override {
            None
        } else {
            primary_credential_pool.cloned()
        },
        skip_primary_credential_pool_fallback: has_runtime_override,
    })
}

/// Run the smart-routing decision: scan the latest user message for
/// keyword triggers, optionally downshift to a cheap-model route, and
/// emit a [`TurnRuntimeRoute`] if a cheap model was selected. Returns
/// `None` when smart routing is disabled or the primary runtime should
/// be used as-is.
pub(crate) fn resolve_smart_runtime_route(
    config: &AgentConfig,
    oauth: &OAuthRefresh,
    primary_credential_pool: Option<&Arc<CredentialPool>>,
    messages: &[Message],
) -> Option<TurnRuntimeRoute> {
    let text = latest_user_text(messages)?;
    let primary = primary_runtime_snapshot(config, primary_credential_pool);
    let outcome = resolve_turn_route(
        text,
        &config.smart_model_routing,
        &primary,
        |cheap, explicit_key| {
            try_build_cheap_runtime(config, oauth, primary_credential_pool, cheap, explicit_key)
        },
    );

    match outcome {
        ResolveTurnOutcome::CheapRouted {
            model,
            label,
            runtime,
            signature,
        } => {
            let cheap = config.smart_model_routing.cheap_model.as_ref()?;
            Some(TurnRuntimeRoute {
                model,
                provider: Some(runtime.provider.clone()),
                base_url: runtime.base_url.clone(),
                api_key_env: cheap
                    .api_key_env
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                api_mode: Some(runtime.api_mode.clone()),
                command: runtime.command.clone(),
                args: runtime.args.clone(),
                credential_pool: runtime.credential_pool.clone(),
                credential_pool_fallback: !runtime.skip_primary_credential_pool_fallback,
                route_label: Some(label),
                routing_reason: Some("simple_turn".to_string()),
                signature,
            })
        }
        ResolveTurnOutcome::Primary { .. } => None,
    }
}

/// Pick the model the cost guard should fall back to when nearing
/// `max_cost_usd`. Precedence:
/// 1. Explicit `cost_guard_degrade_model` config field (if set).
/// 2. Generic `retry.fallback_model` config field (if set).
/// 3. Hard-coded `openai:gpt-4o-mini`, unless the primary model
/// already is `openai:gpt-4o-mini` (in which case there's no
/// cheaper model to pick — return `None`).
pub(crate) fn resolve_cost_degrade_model(config: &AgentConfig) -> Option<String> {
    if let Some(ref m) = config.cost_guard_degrade_model {
        if !m.trim().is_empty() {
            return Some(m.trim().to_string());
        }
    }
    if let Some(ref m) = config.retry.fallback_model {
        if !m.trim().is_empty() {
            return Some(m.trim().to_string());
        }
    }
    if config.model.trim() != "openai:gpt-4o-mini" {
        return Some("openai:gpt-4o-mini".to_string());
    }
    None
}
