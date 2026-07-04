//! Pre-dispatch + per-call tool routing helpers.
//!
//! Phase 5 module reshape (2026-07-04). Mirrors the OAuthRefresh /
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
//! - [`denial_repick_hint`] — when the user denies a tool call,
//!   short prose appended to the deny reason so the next model turn
//!   knows which tool to try instead.
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

use hermix_config::env_compat;
use hermix_core::ToolCall;

use crate::agent_helpers::{ToolDispatchDecision, ToolRegistry};

/// Remove duplicate tool calls that share the same `function.name`
/// and `function.arguments`. Order-stable: first occurrence wins.
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

    let mut args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
        .unwrap_or_else(|_| serde_json::json!({}));
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

/// Build a per-tool hint that we append to a user-denial reason so the
/// LLM, on its next turn, has a concrete suggestion for *which* tool
/// would have been more appropriate.
///
/// Static string table; no `self` dependency. Lives here so
/// `resolve_pre_dispatch` (also here) keeps the hint generation close.
pub(crate) fn denial_repick_hint(tool_name: &str, raw_args: &str) -> &'static str {
    match tool_name {
        "computer_use" => {
            // Parse out the `action` so we can be specific. Default
            // to screenshot if the field is missing.
            let action = serde_json::from_str::<serde_json::Value>(raw_args)
                .ok()
                .and_then(|v| {
                    v.get("action")
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "screenshot".to_string());
            if action == "screenshot" {
                " The user may have denied this because the question can be \
                 answered without a screen capture — try `terminal` (e.g. `dir` \
                 / `ls`), `list_files`, or `read_file` first."
            } else {
                " The user may have denied this because a non-GUI tool would \
                 have worked — try `terminal` first (it can click via the \
                 user32 API too, but most tasks don't need that)."
            }
        }
        "browser" | "browser_use" => {
            " The user may have denied this because the same information is \
             reachable via `terminal` (curl / wget) or `web_extract` — try \
             those first."
        }
        _ => "",
    }
}

/// Resolve the pre-tool-dispatch hook for every tool call up front.
///
/// When `AgentCallbacks::on_pre_tool_dispatch` is set, this invokes the
/// hook once per tool call. `Allow` and `Deny` decisions resolve
/// synchronously; `AwaitApproval` blocks on a `oneshot::Receiver` until
/// the upstream layer (e.g. `LocalAgentService`) sends a verdict. The
/// returned map is keyed by `ToolCall::id` and consumed by both the
/// orchestrated `delegate_task` path and the `JoinSet` path.
pub(crate) async fn resolve_pre_dispatch(
    on_pre_tool_dispatch: Option<&(dyn Fn(&ToolCall) -> ToolDispatchDecision + Send + Sync)>,
    tool_calls: &[ToolCall],
) -> HashMap<String, Result<(), String>> {
    let Some(hook) = on_pre_tool_dispatch else {
        return HashMap::new();
    };
    let mut decisions = HashMap::with_capacity(tool_calls.len());
    for tc in tool_calls {
        let id = tc.id.clone();
        let outcome = match hook(tc) {
            ToolDispatchDecision::Allow => Ok(()),
            ToolDispatchDecision::Deny { reason } => Err(reason),
            ToolDispatchDecision::AwaitApproval(rx) => match rx.await {
                Ok(true) => Ok(()),
                Ok(false) => Err(format!(
                    "User denied the tool call.{hint}",
                    hint = denial_repick_hint(&tc.function.name, &tc.function.arguments),
                )),
                Err(_) => Err("Approval channel closed before response.".to_string()),
            },
        };
        decisions.insert(id, outcome);
    }
    decisions
}

/// Max delegation nesting depth, env-overridable. Default 4 when the
/// env var is absent or unparseable.
pub(crate) fn resolve_max_delegate_depth() -> u32 {
    env_compat("HERMIX_MAX_DELEGATE_DEPTH", "HERMES_MAX_DELEGATE_DEPTH")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(4)
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
