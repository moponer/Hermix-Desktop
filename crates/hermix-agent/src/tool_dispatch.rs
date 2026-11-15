//! Pre-dispatch + per-call tool routing helpers.
//!
//! module reshape . Mirrors the OAuthRefresh /
//! CostGuard / ContextPressureMonitor / RetryCounters / runtime_provider
//! / cost_routing sibling layout established earlier today.
//!
//! What's in this module (~150 lines, all `pub(crate)`):
//!
//! - [`deduplicate_tool_calls`] — pure function over `&[ToolCall]`;
//!   removes identical (`name + args`) duplicates.
//! - [`repair_tool_call`] — fuzzy-fix an unknown tool name via
//!   case-insensitive or substring matching against the registered
//!   toolset.
//! - [`hydrate_session_search_args`] — inject the current session id
//!   into `session_search` calls when the field is missing.
//! - [`resolve_pre_dispatch`] — fan out the
//!   `AgentCallbacks::on_pre_tool_dispatch` hook for every tool call
//!   and collect the per-call decisions.
//! - [`resolve_max_delegate_depth`] — small env-overridable constant
//!   governing how deep sub-agent delegation can nest.
//! - [`cap_delegates`] — clamp the number of concurrent `delegate_task`
//!   calls to `AgentConfig::max_concurrent_delegates`.
//!
//! The big 480-line `AgentLoop::execute_tool_calls` itself stays on the
//! impl block — its body reads `self.sub_agent_orchestrator`,
//! `self.tool_registry`, `self.callbacks`, etc. and lifting the whole
//! body out as a free function would require passing 6+ agent-loop
//! fields as explicit arguments. That's a separate "physical-split"
//! step (see MEMORY.md pending list). This commit only moves the
//! pure-function helpers that don't read `self`-internal state.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hermix_config::env_compat;
use hermix_core::{ApprovalOutcome, ToolCall};

use crate::agent_helpers::{ToolDispatchDecision, ToolRegistry};

/// Remove duplicate tool calls that share the same `function.name`
/// and `function.arguments`. Order-stable: first occurrence wins.
/// Schema-`required` pre-check, run immediately before an approval raise.
/// Never ask the user to approve a call that is guaranteed to die in the
/// handler's own param validation (the storage_clean `confirm` incident:
/// approval granted, then "必须要有 confirm: true" — a wasted 600s window
/// on a call that could be rejected for free). Field PRESENCE only — type
/// coercion lives in the registry (`registry::validation::coerce_args`)
/// and value judgment stays with the handler.
pub(crate) fn schema_required_violation(
    registry: &hermix_tools::ToolRegistry,
    tc: &ToolCall,
) -> Option<String> {
    let schema = registry.get_tool_schema(&tc.function.name)?;
    let required = schema.parameters.required.as_ref()?;
    if required.is_empty() {
        return None;
    }
    let params: serde_json::Value =
        serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
    let obj = params.as_object()?;
    let missing: Vec<&str> = required
        .iter()
        .map(|s| s.as_str())
        .filter(|f| !obj.contains_key(*f))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let listed = missing
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Invalid tool parameters: 缺少必需参数 {listed}。         请补全后重新调用 {}（补全前不会触发审批）。", tc.function.name
    ))
}

pub(crate) fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for tc in calls {
        let key = format!("{}:{}", tc.function.name, tc.function.arguments);
        if seen.insert(key) {
            deduped.push(tc.clone());
        } else {
            tracing::warn!("Deduplicated tool call: {}", tc.function.name);
        }
    }
    deduped
}

/// Try to repair an unknown tool name via case-insensitive or
/// substring matching. Returns `true` if `tc.function.name` was
/// rewritten in-place. The fuzzy-match threshold is "target ⊆ name
/// OR name ⊆ target" — both lowercase.
pub(crate) fn repair_tool_call(tool_registry: &ToolRegistry, tc: &mut ToolCall) -> bool {
    if tool_registry.get(&tc.function.name).is_some() {
        return false;
    }
    let names = tool_registry.names();
    let target = tc.function.name.to_lowercase();

    if let Some(name) = names.iter().find(|n| n.to_lowercase() == target) {
        tracing::info!("Repaired tool call: '{}' → '{}'", tc.function.name, name);
        tc.function.name = name.clone();
        return true;
    }

    if let Some(name) = names
        .iter()
        .find(|n| n.to_lowercase().contains(&target) || target.contains(&n.to_lowercase()))
    {
        tracing::info!(
            "Repaired tool call (fuzzy): '{}' → '{}'",
            tc.function.name,
            name
        );
        tc.function.name = name.clone();
        return true;
    }
    false
}

/// Inject `current_session_id` into `session_search` tool calls when
/// the field is missing. Trims whitespace + skips when `session_id`
/// itself is empty. The new args string only round-trips if
/// `tc.function.arguments` was a JSON object.
pub(crate) fn hydrate_session_search_args(session_id: Option<&str>, tc: &mut ToolCall) {
    if tc.function.name != "session_search" {
        return;
    }
    let Some(session_id) = session_id else {
        return;
    };
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return;
    }

    let mut args: serde_json::Value =
        serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    let has_current = obj
        .get("current_session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if has_current {
        return;
    }
    obj.insert(
        "current_session_id".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );
    if let Ok(updated) = serde_json::to_string(&args) {
        tc.function.arguments = updated;
    }
}

/// Resolve the pre-tool-dispatch hook for every tool call up front.
/// When `AgentCallbacks::on_pre_tool_dispatch` is set, this invokes the
/// hook once per tool call. `Allow` and `Deny` decisions resolve
/// synchronously; `AwaitApproval` blocks on a `oneshot::Receiver` until
/// the upstream layer (e.g. `LocalAgentService`) sends a verdict. The
/// returned map is keyed by `ToolCall::id` and consumed by both the
/// orchestrated `delegate_task` path and the `JoinSet` path.
/// (type-B root-cause): when `shutdown` is `Some`, the
/// `AwaitApproval` arm races the oneshot against `Notify::notified()`
/// so a server-side WebSocket disconnect unblocks the agent task as
/// `ApprovalOutcome::ChannelClosed` instead of stranding it in a bare
/// `rx.await` until the registry timeout fires (or, if the server
/// aborts the task first, until `drop` wakes the receiver with
/// `Err(_)`). The denial note is then written to the context, the
/// next `collect_stream_llm_response` runs, and `stream_first_chunk_timeout_secs`
/// brings the whole turn to a clean `Done` within ~30s of the
/// disconnect.
pub(crate) async fn resolve_pre_dispatch(
    on_pre_tool_dispatch: Option<&(dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync)>,
    tool_calls: &[ToolCall],
    shutdown: Option<&Arc<tokio::sync::Notify>>,
    on_pre_tool_approved: Option<&Arc<dyn Fn(&ToolCall) + Send + Sync>>,
) -> HashMap<String, Result<(), ToolDenial>> {
    let Some(hook) = on_pre_tool_dispatch else {
        return HashMap::new();
    };
    let mut decisions = HashMap::with_capacity(tool_calls.len());
    // Phase 1: call the hook for every tool call and hand back the
    // future that waits on it. The hook is synchronous and emits the
    // approval card as a side effect, so building all these futures
    // before awaiting any of them is what makes the cards appear
    // together. The old shape awaited each `rx` inside the loop body,
    // so card N+1 was not rendered until card N was answered.
    let pending: Vec<_> = tool_calls
        .iter()
        .map(|tc| async move {
        let id = tc.id.clone();
        let tool_name = tc.function.name.clone();
        let outcome = match hook(tc) {
            ToolDispatchDecision::Allow => {
                tracing::info!(
                    tool = %tool_name,
                    tool_call_id = %id,
                    "resolve_pre_dispatch: Allow",
                );
                Ok(())
            }
            ToolDispatchDecision::Deny { reason } => {
                tracing::info!(
                    tool = %tool_name,
                    tool_call_id = %id,
                    reason = %reason,
                    "resolve_pre_dispatch: Deny",
                );
                Err(ToolDenial {
                    kind: DenialKind::Policy,
                    reason,
                })
            }
            ToolDispatchDecision::AwaitApproval(rx) => {
                tracing::info!(
                    tool = %tool_name,
                    tool_call_id = %id,
                    "resolve_pre_dispatch: AwaitApproval — blocking until user resolves",
                );
                // Race: registry timeout vs server-side
                // WebSocket-disconnect soft-shutdown. Without this race,
                // a stale-click + disconnect leaves the agent task
                // parked here until the server `abort()`s it (which
                // also kills any chance of writing the denial note to
                // the context before exit).
                //
                // 等待的有界性由两层保证：
                //   1. 主层：ApprovalRegistry 按 `ApprovalConfig.timeout_secs`
                //      （默认 600s，0=永不）在到期时发 `TimedOut` 解锁 `rx`；
                //   2. 兜底层（本 select 的 safety_net）：只防"审批事件丢失 /
                //      前端没渲染卡片"这类 rx 永远不会resolve 的故障。
                //      兜底 = 主超时 + 60s 余量（主层先到，兜底永远不与它
                //      竞争）；主超时为 0（用户显式要求永不超时）时退回
                //      24h——旧行为的语义原样保留。
                let (_, net_secs) = approval_wait_bounds(hermix_config::approval_timeout_secs());
                let resolved = match shutdown {
                    Some(notify) => {
                        let notified = notify.notified();
                        tokio::pin!(notified);
                        let safety_net =
                            tokio::time::sleep(std::time::Duration::from_secs(net_secs));
                        tokio::pin!(safety_net);
                        tokio::select! {
                            biased;
                            _ = notified => Ok(ApprovalOutcome::ChannelClosed),
                            res = rx => res,
                            _ = &mut safety_net => {
                                tracing::warn!(
                                    tool = %tool_name,
                                    tool_call_id = %id,
                                    net_secs,
                                    "resolve_pre_dispatch: safety-net timeout — \
                                     request_approval event may have been lost or \
                                     frontend didn't render the card; auto-denying \
                                     to unblock the agent",
                                );
                                Ok(ApprovalOutcome::TimedOut { timeout_secs: net_secs })
                            }
                        }
                    }
                    None => {
                        // HTTP 发送路径没有 shutdown 通道，但等待必须有界：
                        // 与 WS 分支同一把兜底网（主超时由 registry 兜）。
                        let safety_net =
                            tokio::time::sleep(std::time::Duration::from_secs(net_secs));
                        tokio::pin!(safety_net);
                        tokio::select! {
                            biased;
                            res = rx => res,
                            _ = &mut safety_net => {
                                tracing::warn!(
                                    tool = %tool_name,
                                    tool_call_id = %id,
                                    net_secs,
                                    "resolve_pre_dispatch: safety-net timeout (http path) — \
                                     auto-denying to unblock the agent",
                                );
                                Ok(ApprovalOutcome::TimedOut { timeout_secs: net_secs })
                            }
                        }
                    }
                };
                let outcome = match resolved {
                    Ok(ApprovalOutcome::Approved) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            "resolve_pre_dispatch: AwaitApproval resolved — Approved",
                        );
                        // P6 direction 3: notify caller that the user
                        // approved this resource so it can update the
                        // declarative permission profile (next time the
                        // same path/command is requested, no prompt).
                        if let Some(f) = on_pre_tool_approved {
                            f(tc);
                        }
                        Ok(())
                    }
                    Ok(ApprovalOutcome::DeniedWithFeedback { note }) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            note = %note,
                            "resolve_pre_dispatch: AwaitApproval resolved — DeniedWithFeedback",
                        );
                        Err(ToolDenial {
                            kind: DenialKind::UserDenied,
                            reason: format!(
                                "User denied the tool call and said: {note}.",
                            ),
                        })
                    }
                    Ok(ApprovalOutcome::DeniedByUser) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            "resolve_pre_dispatch: AwaitApproval resolved — DeniedByUser",
                        );
                        Err(ToolDenial {
                            kind: DenialKind::UserDenied,
                            reason: "User denied the tool call.".to_string(),
                        })
                    }
                    Ok(ApprovalOutcome::TimedOut { timeout_secs }) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            timeout_secs,
                            "resolve_pre_dispatch: AwaitApproval resolved — TimedOut",
                        );
                        Err(ToolDenial {
                            kind: DenialKind::TimedOut,
                            reason: format!(
                                "The {timeout_secs}-second approval window expired before a decision \
                                 was made, so the tool was not executed (the agent defaulted to \
                                 deny)."
                            ),
                        })
                    }
                    Ok(ApprovalOutcome::ChannelClosed) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            "resolve_pre_dispatch: AwaitApproval resolved — ChannelClosed",
                        );
                        Err(ToolDenial {
                            kind: DenialKind::ChannelClosed,
                            reason: "Approval channel closed before a response arrived (the \
                                     connection dropped, the server restarted, or the session was \
                                     reloaded). The tool was not executed."
                                .to_string(),
                        })
                    }
                    Err(_) => {
                        tracing::info!(
                            tool = %tool_name,
                            tool_call_id = %id,
                            "resolve_pre_dispatch: AwaitApproval resolved — ChannelClosed (rx error)",
                        );
                        Err(ToolDenial {
                            kind: DenialKind::ChannelClosed,
                            reason: "Approval channel closed before a response arrived (the \
                                     connection dropped, the server restarted, or the session was \
                                     reloaded). The tool was not executed."
                                .to_string(),
                        })
                    }
                };
                outcome
            }
        };
    (id, outcome)
        })
        .collect();

    // Phase 2: resolve them concurrently. Each arm keeps the original
    // semantics (registry timeout raced against the soft-shutdown
    // notify plus a safety net) — this is a scheduling change only.
    for (id, outcome) in futures::future::join_all(pending).await {
        decisions.insert(id, outcome);
    }
    tracing::info!(
        total = decisions.len(),
        allowed = decisions.values().filter(|r| r.is_ok()).count(),
        denied = decisions.values().filter(|r| r.is_err()).count(),
        "resolve_pre_dispatch: all tool calls resolved",
    );
    decisions
}

/// Why a tool call was not dispatched. Carried through the
/// `resolve_pre_dispatch` map and consumed by the tool-result processor
/// to (a) build an accurate `tool_result` reason and (b) decide whether
/// a structured denial system note should be injected into the context
/// (see [`crate::tool_result_processor::push_tool_result`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DenialKind {
    /// A policy gate (hardline blocklist, Tirith, risk level) blocked
    /// the call before it ever reached the user.
    Policy,
    /// The user explicitly clicked "拒绝".
    UserDenied,
    /// The registry's default-deny timer fired before a decision.
    TimedOut,
    /// The sender side dropped (WS/connection/server/session loss).
    ChannelClosed,
}

impl DenialKind {
    /// Stable, lowercase, snake_case identifier mirrored by the
    /// `ToolResult::denial_kind` field and the frontend banner's
    /// outcome codes. Additive only.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            DenialKind::Policy => "policy",
            DenialKind::UserDenied => "user_denied",
            DenialKind::TimedOut => "timed_out",
            DenialKind::ChannelClosed => "channel_closed",
        }
    }

    /// Map the persisted `ToolResult::denial_kind` string back to the
    /// enum. Unknown strings fall back to `ChannelClosed` (the safest
    /// "something went wrong, no deliberate user intent" category) so
    /// the "tell the user" note still fires for forward-compat values.
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "policy" => DenialKind::Policy,
            "user_denied" => DenialKind::UserDenied,
            "timed_out" => DenialKind::TimedOut,
            "channel_closed" => DenialKind::ChannelClosed,
            _ => DenialKind::ChannelClosed,
        }
    }

    /// True for the user-facing denials that require the "tell the user
    /// what happened + ask how to proceed" system note. Policy denials
    /// (Tirith/hardline) are silent-by-design — the LLM already knows
    /// the policy blocked it from the `reason`.
    pub(crate) fn requires_user_notice(&self) -> bool {
        matches!(
            self,
            DenialKind::UserDenied | DenialKind::TimedOut | DenialKind::ChannelClosed
        )
    }
}

/// A structured, non-policy tool-call denial. `kind` drives the
/// user-facing message phrasing; `reason` is what lands in the LLM's
/// `tool_result` (and therefore in the model context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolDenial {
    pub kind: DenialKind,
    pub reason: String,
}

/// Pure decision table for the approval wait window (P2-6 scheduling
/// extraction). Returns `(primary_secs, net_secs)`:
/// - primary = the user-configured approval timeout (`0` = never expire
///   by intent);
/// - safety net = primary + 60s margin so the registry's own timeout
///   always wins the race; when primary is `0` the net falls back to
///   24h so a lost approval event still cannot park the loop forever.
pub(crate) fn approval_wait_bounds(primary_secs: u64) -> (u64, u64) {
    let net_secs = if primary_secs == 0 {
        86_400
    } else {
        primary_secs.saturating_add(60)
    };
    (primary_secs, net_secs)
}

/// Max delegation nesting depth, env-overridable. Default falls back to the
/// declared safety authority (`sub_agent_safety::MAX_DELEGATE_DEPTH = 2`) —
/// the two used to drift (执行默认 4，声明权威 2)，代理树按深度指数放大
/// 请求与费用。确需更深层的场景用 env 显式放大。
pub(crate) fn resolve_max_delegate_depth() -> u32 {
    env_compat("HERMIX_MAX_DELEGATE_DEPTH", "HERMES_MAX_DELEGATE_DEPTH")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(crate::sub_agent_safety::MAX_DELEGATE_DEPTH)
}

/// Cap concurrent `delegate_task` calls based on config. Non-delegate
/// calls are passed through untouched. The cap is per-batch — the
/// caller (AgentLoop::execute_tool_calls) iterates over a single
/// resolved batch.
pub(crate) fn cap_delegates(max_concurrent_delegates: u32, tool_calls: &mut Vec<ToolCall>) {
    let delegate_count = tool_calls
        .iter()
        .filter(|tc| tc.function.name == "delegate_task")
        .count() as u32;
    if delegate_count > max_concurrent_delegates {
        tracing::warn!(
            "Capping delegate_task calls from {} to {}",
            delegate_count,
            max_concurrent_delegates
        );
        let mut kept_delegates = 0u32;
        tool_calls.retain(|tc| {
            if tc.function.name == "delegate_task" {
                kept_delegates += 1;
                kept_delegates <= max_concurrent_delegates
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{oneshot, Notify};

    use hermix_core::{ApprovalOutcome, FunctionCall, JsonSchema, ToolCall};

    struct OkHandler;

    #[async_trait::async_trait]
    impl hermix_tools::ToolHandler for OkHandler {
        fn schema(&self) -> hermix_core::ToolSchema {
            hermix_core::tool_schema("ok", "test", JsonSchema::new("object"))
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
        ) -> Result<String, hermix_core::ToolError> {
            Ok("ok".to_string())
        }
    }

    fn strict_required_registry() -> hermix_tools::ToolRegistry {
        let r = hermix_tools::ToolRegistry::new();
        r.register_with_risk(
            "strict_tool",
            "test",
            hermix_core::tool_schema(
                "strict_tool",
                "test",
                JsonSchema {
                    schema_type: Some("object".to_string()),
                    properties: None,
                    required: Some(vec!["confirm".to_string()]),
                    additional_properties: None,
                },
            ),
            Arc::new(OkHandler),
            Arc::new(|| true),
            vec![],
            true,
            "strict stub",
            "🔒",
            None,
            hermix_core::RiskLevel::Dangerous,
            false,
        );
        r
    }

    fn lax_registry() -> hermix_tools::ToolRegistry {
        let r = hermix_tools::ToolRegistry::new();
        r.register_with_risk(
            "lax_tool",
            "test",
            hermix_core::tool_schema("lax_tool", "test", JsonSchema::new("object")),
            Arc::new(OkHandler),
            Arc::new(|| true),
            vec![],
            true,
            "lax stub",
            "🟢",
            None,
            hermix_core::RiskLevel::Safe,
            false,
        );
        r
    }

    #[test]
    fn schema_required_violation_flags_missing_required_field() {
        let reg = strict_required_registry();
        let err = schema_required_violation(&reg, &tc("strict_tool", r#"{"paths":[1]}"#))
            .expect("missing confirm must be flagged before any approval is raised");
        assert!(err.contains("confirm"), "error must name the field: {err}");
    }

    #[test]
    fn schema_required_violation_passes_complete_args() {
        let reg = strict_required_registry();
        assert!(schema_required_violation(&reg, &tc("strict_tool", r#"{"confirm": true}"#)).is_none());
    }

    #[test]
    fn schema_required_violation_ignores_tools_without_required_list() {
        let reg = lax_registry();
        assert!(schema_required_violation(&reg, &tc("lax_tool", r#"{}"#)).is_none());
    }

    #[test]
    fn schema_required_violation_ignores_unknown_tools() {
        let reg = strict_required_registry();
        assert!(schema_required_violation(&reg, &tc("not_registered", r#"{}"#)).is_none());
    }

    use crate::agent_helpers::ToolDispatchDecision;

    // Pull in the parent module's `DenialKind`, `ToolDenial`, and
    // `resolve_pre_dispatch` so the denial-mapping tests below can call
    // and assert on them directly.
    use super::*;

    /// Stable, canonical key for cross-turn tool-call deduplication.
    /// Moved to `#[cfg(test)]` — the cross-turn dedup feature was removed
    /// , so this function only lives on for its test.
    fn tool_call_dedup_key(tc: &ToolCall) -> String {
        let name = tc.function.name.to_lowercase();
        let args = tc.function.arguments.trim();
        let canonical = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| args.to_string());
        format!("{}:{}", name, canonical)
    }

    fn tc(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: format!("call-{}", name),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn dedup_key_is_canonical_and_case_insensitive() {
        // Reordered JSON keys must collide (canonical sort).
        let a = tc("fetch_webpage", r#"{"url":"https://x","mode":"fast"}"#);
        let b = tc("fetch_webpage", r#"{"mode":"fast","url":"https://x"}"#);
        assert_eq!(
            tool_call_dedup_key(&a),
            tool_call_dedup_key(&b),
            "reordered JSON args should produce the same dedup key"
        );

        // Different casing on the tool name must collide.
        let c = tc("FetchWebpage", r#"{"url":"https://x"}"#);
        let d = tc("fetchWebpage", r#"{"url":"https://x"}"#);
        assert_eq!(
            tool_call_dedup_key(&c),
            tool_call_dedup_key(&d),
            "tool name casing should not affect the dedup key"
        );

        // Different args must NOT collide.
        let e = tc("fetch_webpage", r#"{"url":"https://x"}"#);
        let f = tc("fetch_webpage", r#"{"url":"https://y"}"#);
        assert_ne!(
            tool_call_dedup_key(&e),
            tool_call_dedup_key(&f),
            "different URLs must produce distinct dedup keys"
        );
    }

    // ---- DenialKind / ToolDenial taxonomy (A+C product fix) ---------

    #[test]
    fn denial_kind_strings_and_labels_are_stable() {
        assert_eq!(DenialKind::Policy.as_str(), "policy");
        assert_eq!(DenialKind::UserDenied.as_str(), "user_denied");
        assert_eq!(DenialKind::TimedOut.as_str(), "timed_out");
        assert_eq!(DenialKind::ChannelClosed.as_str(), "channel_closed");
    }

    #[test]
    fn approval_wait_bounds_primary_plus_margin() {
        assert_eq!(approval_wait_bounds(600), (600, 660));
        assert_eq!(approval_wait_bounds(1), (1, 61));
    }

    #[test]
    fn approval_wait_bounds_zero_primary_falls_back_to_24h() {
        // User explicitly asked for "never time out" — the safety net
        // still must be bounded so a lost approval event can't park the
        // loop forever.
        assert_eq!(approval_wait_bounds(0), (0, 86_400));
    }

    #[test]
    fn only_policy_denial_skips_user_notice() {
        assert!(!DenialKind::Policy.requires_user_notice());
        assert!(DenialKind::UserDenied.requires_user_notice());
        assert!(DenialKind::TimedOut.requires_user_notice());
        assert!(DenialKind::ChannelClosed.requires_user_notice());
    }

    // ---- resolve_pre_dispatch: ApprovalOutcome → ToolDenial --------

    /// Build a hook that, on each call, returns a fresh
    /// `AwaitApproval` wrapping a pre-resolved receiver carrying
    /// `outcome`. Constructing a new receiver per call sidesteps the
    /// fact that `oneshot::Receiver` is not `Clone` (so we can't clone
    /// a `ToolDispatchDecision` cheaply) — fine for these single-call
    /// tests.
    fn hook_awaiting(
        outcome: ApprovalOutcome,
    ) -> Box<dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync> {
        Box::new(move |_tc| ToolDispatchDecision::AwaitApproval(pre_resolved_rx(outcome.clone())))
    }

    fn pre_resolved_rx(outcome: ApprovalOutcome) -> oneshot::Receiver<ApprovalOutcome> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(outcome);
        rx
    }

    #[tokio::test]
    async fn awaited_approval_denied_maps_to_user_denied() {
        let tc = tc("storage_clean", r#"{"paths":["/tmp/x"]}"#);
        let map = resolve_pre_dispatch(
            Some(&hook_awaiting(ApprovalOutcome::DeniedByUser)),
            &[tc],
            None,
            None,
        )
        .await;
        let err = map.get("call-storage_clean").unwrap().as_ref().unwrap_err();
        assert_eq!(err.kind, DenialKind::UserDenied);
        assert!(err.reason.contains("User denied the tool call"));
    }

    #[tokio::test]
    async fn awaited_approval_timed_out_maps_to_timed_out() {
        let tc = tc("storage_clean", r#"{"paths":["/tmp/x"]}"#);
        let map = resolve_pre_dispatch(
            Some(&hook_awaiting(ApprovalOutcome::TimedOut {
                timeout_secs: 86_400,
            })),
            &[tc],
            None,
            None,
        )
        .await;
        let err = map.get("call-storage_clean").unwrap().as_ref().unwrap_err();
        assert_eq!(err.kind, DenialKind::TimedOut);
        assert!(err.reason.contains("approval window expired"));
    }

    #[tokio::test]
    async fn awaited_approval_channel_closed_maps_to_channel_closed() {
        let tc = tc("storage_clean", r#"{"paths":["/tmp/x"]}"#);
        let map = resolve_pre_dispatch(
            Some(&hook_awaiting(ApprovalOutcome::ChannelClosed)),
            &[tc],
            None,
            None,
        )
        .await;
        let err = map.get("call-storage_clean").unwrap().as_ref().unwrap_err();
        assert_eq!(err.kind, DenialKind::ChannelClosed);
        assert!(err.reason.contains("Approval channel closed"));
    }

    #[tokio::test]
    async fn awaited_approval_approved_maps_to_ok() {
        let tc = tc("storage_clean", r#"{"paths":["/tmp/x"]}"#);
        let map = resolve_pre_dispatch(
            Some(&hook_awaiting(ApprovalOutcome::Approved)),
            &[tc],
            None,
            None,
        )
        .await;
        assert!(map.get("call-storage_clean").unwrap().is_ok());
    }

    /// (type-B root-cause): when the server fires the
    /// shutdown `Notify` mid-await, the agent loop should see
    /// `ChannelClosed` instead of stranding on the bare `rx.await`.
    /// The pre-resolved receiver here is irrelevant — the `Notify`
    /// arm wins via `biased` selection before the receiver ever
    /// gets a chance to fire.
    #[tokio::test]
    async fn shutdown_notify_unblocks_await_approval_as_channel_closed() {
        let tc = tc("storage_clean", r#"{"paths":["/tmp/x"]}"#);
        // Shared between hook (notify_one on each call) and the
        // outer `resolve_pre_dispatch` call (which registers the
        // select! waiter). Pre-arming inside the hook keeps the
        // test deterministic across the call ordering below.
        let notify = Arc::new(Notify::new());
        let hook: Box<dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync> = Box::new({
            let notify = notify.clone();
            move |_tc| {
                let (tx, rx) = oneshot::channel::<ApprovalOutcome>();
                drop(tx);
                notify.notify_one();
                ToolDispatchDecision::AwaitApproval(rx)
            }
        });
        let map = resolve_pre_dispatch(Some(&hook), &[tc], Some(&notify), None).await;
        let err = map.get("call-storage_clean").unwrap().as_ref().unwrap_err();
        assert_eq!(
            err.kind,
            DenialKind::ChannelClosed,
            "shutdown Notify must short-circuit the approval wait as ChannelClosed",
        );
    }

    /// P6 direction 3: when a tool call's approval resolves to
    /// `Approved`, the `on_pre_tool_approved` hook fires with the
    /// original `ToolCall` so the caller can update its permission
    /// profile.
    #[tokio::test]
    async fn awaited_approval_approved_fires_on_pre_tool_approved_hook() {
        use std::sync::{Arc, Mutex};
        let tc = tc("read_file", r#"{"path":"C:\\Users\\alice\\x.txt"}"#);
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let dispatch_hook: Box<dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync> =
            Box::new(|_tc| {
                ToolDispatchDecision::AwaitApproval(pre_resolved_rx(ApprovalOutcome::Approved))
            });
        let approved_hook: Arc<dyn Fn(&ToolCall) + Send + Sync> = {
            let seen = seen.clone();
            Arc::new(move |tc: &ToolCall| {
                seen.lock().unwrap().push(tc.function.name.clone());
            })
        };
        let map =
            resolve_pre_dispatch(Some(&dispatch_hook), &[tc], None, Some(&approved_hook)).await;
        assert!(map.get("call-read_file").unwrap().is_ok());
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &["read_file".to_string()],
            "on_pre_tool_approved must fire once with the approved ToolCall"
        );
    }

    /// P6 direction 3: the hook must NOT fire on a denied approval.
    #[tokio::test]
    async fn denied_approval_does_not_fire_on_pre_tool_approved_hook() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tc = tc("read_file", r#"{"path":"C:\\Users\\alice\\x.txt"}"#);
        let count = Arc::new(AtomicUsize::new(0));
        let dispatch_hook: Box<dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync> =
            Box::new(|_tc| {
                ToolDispatchDecision::AwaitApproval(pre_resolved_rx(ApprovalOutcome::DeniedByUser))
            });
        let approved_hook: Arc<dyn Fn(&ToolCall) + Send + Sync> = {
            let count = count.clone();
            Arc::new(move |_tc: &ToolCall| {
                count.fetch_add(1, Ordering::SeqCst);
            })
        };
        let map =
            resolve_pre_dispatch(Some(&dispatch_hook), &[tc], None, Some(&approved_hook)).await;
        assert!(map.get("call-read_file").unwrap().is_err());
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "on_pre_tool_approved must not fire on denial"
        );
    }
}
