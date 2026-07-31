//! Cost-guard bookkeeping — tracks session spend across turns and
//! decides when to warn or trip the configured `max_cost_usd`.
//!
//! : the 4 sibling fields (`accumulated_usage`,
//! `session_cost_usd`, `cost_warned`, `forced_runtime_route`) used to
//! live directly on `LoopState` and were passed one-by-one as 4 sibling
//! `&mut` arguments into [`AgentLoop::process_post_response`](crate::agent_loop::AgentLoop).
//! Folding them into one struct shrinks `LoopState` from 14 to 10
//! fields and makes `process_post_response`'s `publish_from` shrink
//! another 4 args.
//!
//! : the `forced_runtime_route` field was moved
//! out — it is a routing decision, not a spend-tracking value. It now
//! lives in [`crate::cost_routing::RouteState`], a sibling manager in
//! `LoopState`. `evaluate_thresholds` reads `RouteState` via `&RouteState`
//! and `install_downgrade` became a method on `RouteState`.
//!
//! Physically split out of `agent_loop.rs` so the file
//! that owns the type also owns the type — no API change to callers
//! (`state.cost_guard.*` call paths work unchanged).
//!
//! Fields stay `pub(crate)` so `agent_loop.rs` runtime paths (which
//! read `state.cost_guard.accumulated_usage` etc. at publish time)
//! keep compiling without accessor-method churn.

use hermix_core::UsageStats;

use crate::agent_config::AgentConfig;
use crate::cost_routing::RouteState;

/// Cost-guard bookkeeping consolidated into one struct. The three
/// sibling fields (`accumulated_usage`, `session_cost_usd`,
/// `cost_warned`) mutate in place via `record_usage` /
/// `evaluate_thresholds`. The forced-route state lives in
/// [`RouteState`] — see the note in the module docs.
#[derive(Debug)]
pub struct CostGuard {
    /// Cumulative usage stats across turns.
    pub(crate) accumulated_usage: Option<UsageStats>,
    /// Running session cost estimate in USD.
    pub(crate) session_cost_usd: f64,
    /// Whether we've already warned about exceeding the cost threshold.
    pub(crate) cost_warned: bool,
}

impl Default for CostGuard {
    fn default() -> Self {
        Self {
            accumulated_usage: None,
            session_cost_usd: 0.0,
            cost_warned: false,
        }
    }
}

/// Fallback cost-per-million-USD pricing for known model families
/// when [`AgentConfig`] does not pin a rate.
/// used by `estimate_usage_cost_usd` in the
/// cost guard. Physically moved with the cost guard
/// because no other call site in `agent_loop.rs` needs it.
pub(crate) fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

/// Estimate the USD cost of one turn's `UsageStats` given the model
/// and pricing config. Returns `Some(0.0+)` when pricing is known;
/// `None` only when neither config nor the fallback table has an
/// entry (provider returned no usage AND model is unknown).
pub(crate) fn estimate_usage_cost_usd(
    usage: &UsageStats,
    model: &str,
    config: &AgentConfig,
) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
pub(crate) fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            cache_read_tokens: prev.cache_read_tokens + new.cache_read_tokens,
            cache_creation_tokens: prev.cache_creation_tokens + new.cache_creation_tokens,
            reasoning_tokens: prev.reasoning_tokens + new.reasoning_tokens,
        },
        None => new.clone(),
    }
}

impl CostGuard {
    /// Merge a per-turn `UsageStats` into the running total and
    /// accumulate the per-turn USD cost into `session_cost_usd` using
    /// the model's pricing config. No-op when the provider did not
    /// return usage for the turn.
    /// Pre-extraction this block was open-coded inside
    /// `process_post_response` immediately after the post-LLM hook;
    /// it stays identical otherwise.
    pub fn record_usage(
        &mut self,
        turn_usage: Option<&UsageStats>,
        model: &str,
        config: &AgentConfig,
    ) {
        if let Some(usage) = turn_usage {
            self.accumulated_usage = Some(merge_usage(self.accumulated_usage.clone(), usage));
            if let Some(cost) = estimate_usage_cost_usd(usage, model, config) {
                self.session_cost_usd += cost;
            }
        }
    }

    /// Check whether the warn or trip threshold has been crossed, and
    /// fire the user-visible side effects (downgrade + system message)
    /// the first time the warn threshold is hit. The caller is
    /// responsible for actually building the `TurnRuntimeRoute` from
    /// the returned `model` (the struct doesn't have access to
    /// `AgentLoop::turn_route_cost_guard` which depends on
    /// `self.primary_runtime_snapshot()`).
    /// takes `&RouteState` so it can skip the
    /// warn path when the session has already been downgraded. The
    /// downgrade state itself is owned by `RouteState`, not by this
    /// struct — passing `&RouteState` keeps `CostGuard` focused on
    /// spend accounting.
    pub(crate) fn evaluate_thresholds(
        &mut self,
        route_state: &RouteState,
        max_cost_usd: Option<f64>,
        degrade_at_ratio: f64,
        resolve_degrade_model: impl FnOnce() -> Option<String>,
    ) -> CostGuardAction {
        let Some(limit) = max_cost_usd else {
            return CostGuardAction { warn: None, trip: None };
        };
        // Mirror pre-extraction control flow: the warn branch runs
        // first (flipping `cost_warned` and producing a side effect),
        // then the trip branch is checked independently. Both can
        // fire on the same call (spend jumped past both thresholds
        // in one turn) — caller fires both side effects in order.
        let mut warn = None;
        if !self.cost_warned && self.session_cost_usd >= limit * degrade_at_ratio {
            self.cost_warned = true;
            if route_state.forced_runtime_route.is_none() {
                if let Some(model) = resolve_degrade_model() {
                    warn = Some(WarnEffect {
                        kind: WarnKind::Downgrade { model },
                        session_cost_usd: self.session_cost_usd,
                        limit,
                    });
                } else {
                    warn = Some(WarnEffect {
                        kind: WarnKind::Warn,
                        session_cost_usd: self.session_cost_usd,
                        limit,
                    });
                }
            }
        }
        let trip = if self.session_cost_usd >= limit {
            Some(TripEffect {
                session_cost_usd: self.session_cost_usd,
                limit,
            })
        } else {
            None
        };
        CostGuardAction { warn, trip }
    }
}

/// Result of [`CostGuard::evaluate_thresholds`]. The caller drives
/// the side effects (route build + system message) from the variant.
/// `warn` and `trip` can co-exist on the same call when spend jumped
/// past both thresholds in one turn (e.g. the per-turn usage report
/// pushed total spend above `max_cost_usd` while also clearing the
/// warn ratio). The caller fires the warn side effect (system
/// message, route downgrade) and then the trip side effect
/// (terminating system message + return `CostGuardTripped`), in that
/// order — mirroring the pre-extraction control flow where both
/// branches ran independently.
#[derive(Debug, PartialEq)]
pub struct CostGuardAction {
    /// Did this call cross the warn ratio for the first time?
    /// `None` = no warn side effect. `Some(Downgrade { model, .. })` =
    /// caller should build a `TurnRuntimeRoute` from the model and
    /// install it via [`CostGuard::install_downgrade`], then emit the
    /// "switching to cheaper model" system message. `Some(Warn { .. })`
    /// = caller should emit a "cost guard warning" system message
    /// (no cheap model was configured).
    pub(crate) warn: Option<WarnEffect>,
    /// Did this call exceed the absolute spend limit? If so, the
    /// caller emits a "cost guard tripped" system message and returns
    /// `PostResponseAction::CostGuardTripped`.
    pub(crate) trip: Option<TripEffect>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct WarnEffect {
    pub(crate) kind: WarnKind,
    pub(crate) session_cost_usd: f64,
    pub(crate) limit: f64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum WarnKind {
    Downgrade { model: String },
    Warn,
}

#[derive(Debug, PartialEq)]
pub(crate) struct TripEffect {
    pub(crate) session_cost_usd: f64,
    pub(crate) limit: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::TurnRuntimeRoute;
    use crate::smart_model_routing::{ApiMode, TurnRouteSignature};

    fn empty_route() -> TurnRuntimeRoute {
        TurnRuntimeRoute {
            model: "gpt-4o-mini".into(),
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: Some(ApiMode::ChatCompletions),
            command: None,
            args: Vec::new(),
            credential_pool: None,
            credential_pool_fallback: false,
            route_label: None,
            routing_reason: None,
            signature: TurnRouteSignature {
                model: "gpt-4o-mini".into(),
                provider: None,
                base_url: None,
                api_mode: ApiMode::ChatCompletions,
                command: None,
                args: Vec::new(),
            },
        }
    }

    #[test]
    fn evaluate_returns_no_action_when_limit_is_none() {
        let mut g = CostGuard::default();
        let rs = RouteState::default();
        let a = g.evaluate_thresholds(&rs, None, 0.8, || None);
        assert_eq!(a, CostGuardAction { warn: None, trip: None });
    }

    #[test]
    fn evaluate_warns_with_downgrade_when_model_resolver_succeeds() {
        let mut g = CostGuard::default();
        g.session_cost_usd = 9.0;
        let rs = RouteState::default();
        let a = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || Some("gpt-4o-mini".into()));
        // warn crossed, trip not crossed
        assert!(a.warn.is_some());
        assert!(a.trip.is_none());
        assert!(matches!(
            a.warn.as_ref().unwrap().kind,
            WarnKind::Downgrade { .. }
        ));
        // cost_warned flipped — second call won't re-warn
        let a2 = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || Some("gpt-4o-mini".into()));
        assert_eq!(a2, CostGuardAction { warn: None, trip: None });
    }

    #[test]
    fn evaluate_warns_plainly_when_model_resolver_returns_none() {
        let mut g = CostGuard::default();
        g.session_cost_usd = 9.0;
        let rs = RouteState::default();
        let a = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || None);
        assert!(matches!(a.warn.as_ref().unwrap().kind, WarnKind::Warn));
    }

    #[test]
    fn evaluate_trips_only_when_session_passes_limit() {
        let mut g = CostGuard::default();
        g.session_cost_usd = 5.0;
        let rs = RouteState::default();
        // under limit
        let a = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || None);
        assert_eq!(a, CostGuardAction { warn: None, trip: None });
        // over limit, no downgrade model
        g.session_cost_usd = 12.0;
        let a = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || None);
        // past warn ratio AND past limit in same call → warn + trip
        assert!(a.warn.is_some());
        assert!(a.trip.is_some());
        assert_eq!(a.trip.as_ref().unwrap().session_cost_usd, 12.0);
        assert_eq!(a.trip.as_ref().unwrap().limit, 10.0);
    }

    #[test]
    fn evaluate_skips_warn_when_already_holds_downgrade_route() {
        let mut g = CostGuard::default();
        g.session_cost_usd = 9.0;
        let mut rs = RouteState::default();
        rs.install_downgrade(empty_route());
        // downgrade already installed → don't re-warn
        let a = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || Some("gpt-4o-mini".into()));
        assert_eq!(a, CostGuardAction { warn: None, trip: None });
        // cost_warned flipped on the call above; a fresh guard is the
        // only way to verify that the absence-of-route path re-fires.
        let mut g = CostGuard::default();
        g.session_cost_usd = 9.0;
        let mut rs = RouteState::default();
        rs.install_downgrade(empty_route());
        // remove the route WITHOUT crossing the warn threshold...
        rs.forced_runtime_route = None;
        // ...now cross the warn threshold. cost_warned is false → branch enters,
        // forced_route.is_none() is true → warn set.
        let a2 = g.evaluate_thresholds(&rs, Some(10.0), 0.8, || Some("gpt-4o-mini".into()));
        assert!(a2.warn.is_some());
    }

    #[test]
    fn route_state_install_downgrade_stores_route() {
        let mut rs = RouteState::default();
        assert!(rs.forced_runtime_route.is_none());
        rs.install_downgrade(empty_route());
        assert!(rs.forced_runtime_route.is_some());
    }
}
