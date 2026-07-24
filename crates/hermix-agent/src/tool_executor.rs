//! Parallel tool-call batch executor — physically split out of
//! `agent_loop.rs` on so the 6k-line loop file can focus
//! on the orchestration primitives (`run` / `run_stream` /
//! `process_post_response`).
//!
//! The method body moved here verbatim — **no behaviour change**.
//! It is still an `impl AgentLoop` method, so the `self.*` field
//! accesses it makes (`tool_registry`, `sub_agent_orchestrator`,
//! `delegate_depth`) and the helper methods it calls
//! (`resolve_pre_dispatch`, `resolve_max_delegate_depth`,
//! `note_delegation_start`) are all the same as before the split.
//!
//! ## Why a separate file (instead of staying inside `agent_loop.rs`)
//!
//! `tool_dispatch.rs` already houses the *static* tool-dispatch
//! helpers (dedup, hydrate, decision resolution), but the actual
//! execution logic (parallel JoinSet, orchestrated
//! `delegate_task`, interactive sequential pass, batch timeout,
//! per-call timeout) stayed in `agent_loop.rs` as a 478-line
//! method. After this split:
//!
//! - `tool_dispatch.rs` (236 LoC): the helper zoo. No `AgentLoop`
//!   dependency — pure free functions over the data they receive.
//! - `tool_executor.rs` (this file, ~480 LoC): the batch executor.
//!   `impl AgentLoop { pub(crate) async fn execute_tool_calls }`.
//!
//! Both files can now evolve independently; future changes to
//! execution semantics (concurrency caps, fallback routing,
//! per-tool-rate limits) edit `tool_executor.rs` without touching
//! `tool_dispatch.rs`, and vice versa.

use std::collections::HashMap;

use hermix_core::{ToolCall, ToolErrorRecord, ToolResult};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::agent_loop::AgentLoop;

impl AgentLoop {
    /// Execute a batch of tool calls in parallel using a JoinSet.
    #[tracing::instrument(
        name = "execute_tool_calls",
        level = "debug",
        skip_all,
        fields(
            turn = turn,
            batch_size = tool_calls.len(),
            tool_names = tracing::field::Empty,
            error_count = tracing::field::Empty,
            elapsed_ms = tracing::field::Empty,
        )
    )]
    pub(crate) async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        // Enforce max_tool_calls_per_turn limit
        if self.config.max_tool_calls_per_turn > 0 && tool_calls.len() > self.config.max_tool_calls_per_turn as usize {
            let excess = tool_calls.len() - self.config.max_tool_calls_per_turn as usize;
            tracing::warn!(
                "Tool call limit exceeded: {} calls > {} limit. {} tools will be skipped.",
                tool_calls.len(),
                self.config.max_tool_calls_per_turn,
                excess
            );
            // Return error results for excess tool calls
            let mut results: Vec<ToolResult> = Vec::new();
            for tc in tool_calls.iter().skip(self.config.max_tool_calls_per_turn as usize) {
                results.push(ToolResult::err(
                    &tc.id,
                    format!("Tool call limit reached ({}/{}). This tool call was skipped.",
                        self.config.max_tool_calls_per_turn, tool_calls.len())
                ));
            }
            return results;
        }

        let span = tracing::Span::current();
        span.record(
            "tool_names",
            tracing::field::display(
                tool_calls
                    .iter()
                    .map(|t| t.function.name.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        let _t = std::time::Instant::now();
        // Resolve pre-dispatch decisions for every tool call up front.
        // Allow/Deny resolve synchronously; AwaitApproval blocks on the
        // oneshot channel until upstream resolves the approval request.
        let pre_dispatch = self.resolve_pre_dispatch(tool_calls).await;

        let mut join_set = JoinSet::new();
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();

        // Run orchestrated `delegate_task` calls sequentially in the caller's
        // task — this keeps the inner AgentLoop future out of the Send-bound
        // JoinSet and preserves the requested concurrency cap which is already
        // applied upstream via `cap_delegates`.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
            for tc in tool_calls {
                if tc.function.name != "delegate_task" {
                    continue;
                }
                // Honor the pre-dispatch hook before the orchestrator spends work on it.
                if let Some(Err(denial)) = pre_dispatch.get(&tc.id) {
                    orchestrated.push(ToolResult::denied(
                        &tc.id,
                        denial.reason.clone(),
                        denial.kind.as_str(),
                    ));
                    continue;
                }
                // M1.6.2: push a focus-stack frame BEFORE delegating.
                // We pass an empty sub_agent_id — `note_delegation_end`
                // matches the topmost Delegation frame when the id is
                // empty (see its docstring). This decouples push from
                // the orchestrator's internal UUID generation.
                let parsed_for_focus = serde_json::from_str::<Value>(&tc.function.arguments).ok();
                let task_for_focus = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("task"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.note_delegation_start(&task_for_focus, "");
                if current_delegate_depth >= max_delegate_depth {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                // Phase 18+: if the caller passed an explicit
                // `toolset` whitelist in the delegate_task
                // params, refuse any name that's in the
                // sub-agent blocklist. This is a *belt-and-
                // suspenders* check on top of the child
                // registry construction below — we want to
                // catch the bad call early and surface a
                // clear error to the parent LLM.
                if let Some(toolset) = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("toolset"))
                    .and_then(|v| v.as_str())
                {
                    let names: Vec<String> = toolset
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    if let Some(bad) =
                        crate::sub_agent_safety::contains_blocked_tool(&names)
                    {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            crate::sub_agent_safety::block_reason(&bad),
                        ));
                        continue;
                    }
                }
                let parsed: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            format!(
                                "Invalid JSON params for tool 'delegate_task': {}. \
                                 Please retry with valid JSON.",
                                e
                            ),
                        ));
                        continue;
                    }
                };
                let task = parsed
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if task.trim().is_empty() {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        "delegate_task requires non-empty 'task' string.",
                    ));
                    continue;
                }
                let req = crate::sub_agent_orchestrator::SubAgentRequest {
                    task,
                    context: parsed
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    toolset: parsed
                        .get("toolset")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    model: parsed
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    child_depth: current_delegate_depth + 1,
                    max_depth: max_delegate_depth,
                    parent_budget_remaining_usd,
                    max_iterations: parsed
                        .get("max_iterations")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32),
                    // P6-4: Parse role from args; default to General.
                    role: parsed
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(|s| match s {
                            "explore" => Some(
                                crate::sub_agent_orchestrator::SubAgentRole::Explore,
                            ),
                            "plan" => Some(
                                crate::sub_agent_orchestrator::SubAgentRole::Plan,
                            ),
                            "general" => Some(
                                crate::sub_agent_orchestrator::SubAgentRole::General,
                            ),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    // P6-4: Parse parent_task_id from args if present.
                    parent_task_id: parsed
                        .get("parent_task_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    // Parse image/file attachments from args.
                    attachments: parsed
                        .get("attachments")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|a| {
                                    serde_json::from_value::<
                                        crate::sub_agent_orchestrator::ImageAttachment,
                                    >(a.clone())
                                    .ok()
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    // Parse optional output_schema hint.
                    output_schema: parsed
                        .get("output_schema")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                };
                // Orchestrator internally runs the child on its own
                // `tokio::spawn` task, which erases the child future and breaks
                // async recursion between parent / child `execute_tool_calls`.
                let output = orch.execute(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
            }
        }

        // P2.4 (2026-06-30): run remaining interactive tools (currently
        // just `clarify`) sequentially in the caller's task BEFORE
        // launching the JoinSet. This blocks the parallel work until the
        // user has answered the clarifying question — otherwise we'd
        // race the user response and accumulate stale tool results in
        // the LLM context. delegate_task is already handled above by
        // the orchestrator path and is filtered out of this loop.
        for tc in tool_calls {
            if !crate::agent_helpers::is_interactive_tool(&tc.function.name)
                || tc.function.name == "delegate_task"
            {
                continue;
            }
            // Honor pre-dispatch decisions (Allow/Deny/AwaitApproval).
            if let Some(Err(denial)) = pre_dispatch.get(&tc.id) {
                orchestrated.push(ToolResult::denied(
                    &tc.id,
                    denial.reason.clone(),
                    denial.kind.as_str(),
                ));
                continue;
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            let result = match registry.get(&tool_name) {
                Some(entry) => {
                    let params: Value = match serde_json::from_str(&raw_args) {
                        Ok(v) => v,
                        Err(e) => {
                            let err = ToolResult::err(
                                &tool_call_id,
                                format!(
                                    "Invalid JSON params for tool '{}': {}. \
                                     Please check your parameters and retry with valid JSON.",
                                    tool_name, e
                                ),
                            );
                            orchestrated.push(err);
                            continue;
                        }
                    };
                    let handler_result = (entry.handler)(params);
                    match handler_result {
                        Ok(output) => ToolResult::ok(&tool_call_id, output),
                        Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                    }
                }
                None => {
                    let available = registry.names().join(", ");
                    ToolResult::err(
                        &tool_call_id,
                        format!(
                            "Unknown tool '{}'. Available tools: [{}]",
                            tool_name, available
                        ),
                    )
                }
            };
            orchestrated.push(result);
        }

        for tc in tool_calls {
            // Skip `delegate_task` when an orchestrator already handled it.
            if orchestrator.is_some() && tc.function.name == "delegate_task" {
                continue;
            }
            // P2.4 (2026-06-30): interactive tools run sequentially in
            // the orchestrator pass above (delegate_task) or — for clarify
            // — never enter this JoinSet at all. We extract and handle
            // them right after the orchestrator loop. Skipping them here
            // keeps the JoinSet reserved for genuinely-independent work.
            if crate::agent_helpers::is_interactive_tool(&tc.function.name)
                && !matches!(tc.function.name.as_str(), "delegate_task")
            {
                continue;
            }
            // M1.6.2: push a focus-stack frame for non-orchestrated
            // delegate_task BEFORE spawning the JoinSet future.
            // The pop side (`notify_delegations` → `note_delegation_end`)
            // reads the real sub_agent_id from the tool result content
            // and matches by depth — empty push id is intentional and
            // pairs with the topmost-match policy in `note_delegation_end`.
            if !orchestrator.is_some() && tc.function.name == "delegate_task" {
                let parsed_for_focus = serde_json::from_str::<Value>(&tc.function.arguments).ok();
                let task_for_focus = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("task"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.note_delegation_start(&task_for_focus, "");
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            // Clone the per-tool-call decision so the spawned future owns it.
            let pre_decision = pre_dispatch.get(&tool_call_id).cloned();

            let per_call_timeout = std::time::Duration::from_secs(60);
            join_set.spawn(async move {
                // Honor the pre-dispatch hook: skip handler invocation entirely
                // when the user (or upstream policy) denied this tool call.
                if let Some(Err(denial)) = pre_decision.as_ref() {
                    return ToolResult::denied(
                        &tool_call_id,
                        denial.reason.clone(),
                        denial.kind.as_str(),
                    );
                }
                // A5: per-call timeout around the handler. If a tool
                // hangs (e.g. a `terminal` command waiting for stdin
                // that never arrives, a `web_fetch` on a wedged
                // server), this returns an error envelope instead of
                // wedging the whole batch. 60 s matches the LLM-call
                // timeout in `call_llm_with_retry`.
                let handler_fut = async {
                    match registry.get(&tool_name) {
                        Some(entry) => {
                            // Parse arguments
                            let mut params: Value = match serde_json::from_str(&raw_args) {
                                Ok(v) => v,
                                Err(e) => {
                                    let error_msg = format!(
                                        "Invalid JSON params for tool '{}': {}. \
                                         Please check your parameters and retry with valid JSON.",
                                        tool_name, e
                                    );
                                    return ToolResult::err(&tool_call_id, error_msg);
                                }
                            };

                            if tool_name == "delegate_task" {
                                if current_delegate_depth >= max_delegate_depth {
                                    return ToolResult::err(
                                        &tool_call_id,
                                        format!(
                                            "Delegation depth limit reached ({}/{}).",
                                            current_delegate_depth, max_delegate_depth
                                        ),
                                    );
                                }
                                if let Some(obj) = params.as_object_mut() {
                                    obj.insert(
                                        "child_depth".to_string(),
                                        Value::from(current_delegate_depth + 1),
                                    );
                                    obj.insert(
                                        "max_depth".to_string(),
                                        Value::from(max_delegate_depth),
                                    );
                                    if let Some(remaining) = parent_budget_remaining_usd {
                                        obj.insert(
                                            "parent_budget_remaining_usd".to_string(),
                                            Value::from(remaining),
                                        );
                                    }
                                }
                            }

                            // Execute the handler
                            match (entry.handler)(params) {
                                Ok(output) => ToolResult::ok(&tool_call_id, output),
                                Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                            }
                        }
                        None => {
                            let available = registry.names().join(", ");
                            let error_msg = format!(
                                "Unknown tool '{}'. Available tools: [{}]",
                                tool_name, available
                            );
                            ToolResult::err(&tool_call_id, error_msg)
                        }
                    }
                };
                match tokio::time::timeout(per_call_timeout, handler_fut).await {
                    Ok(result) => result,
                    Err(_elapsed) => ToolResult::err(
                        &tool_call_id,
                        format!(
                            "per-call timeout after {}s: tool '{}' did not return in time",
                            per_call_timeout.as_secs(),
                            tool_name,
                        ),
                    ),
                }
            });
        }

        // Collect results in INPUT order. tool_calls.iter().enumerate()
        // gives us a stable position; orchestrated (delegate_task,
        // interactive tools) and the JoinSet (everything else) both
        // place their result into the same fixed-size Vec<Option<…>>.
        // See A5 commit for the order-preservation rationale: the
        // caller (run / run_stream) feeds results back to the LLM in
        // the same order as the tool_calls it produced, so a delayed
        // JoinSet result landing in slot 0 before a fast one landing
        // in slot 2 would scramble the LLM's view of which result
        // corresponds to which tool call.
        let total = tool_calls.len();
        let mut ordered: Vec<Option<ToolResult>> = (0..total).map(|_| None).collect();
        let index_by_id: HashMap<&str, usize> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| (tc.id.as_str(), i))
            .collect();

        for tool_result in orchestrated {
            if tool_result.is_error {
                let tc = tool_calls
                    .iter()
                    .find(|tc| tc.id == tool_result.tool_call_id);
                if let Some(tc) = tc {
                    tool_errors.push(ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: tool_result.content.clone(),
                        turn,
                    });
                }
            }
            if let Some(&idx) = index_by_id.get(tool_result.tool_call_id.as_str()) {
                ordered[idx] = Some(tool_result);
            } else {
                // An orchestrated result without a matching input id
                // would be a programming error — surface it rather
                // than silently drop the result.
                tracing::error!(
                    tool_call_id = %tool_result.tool_call_id,
                    "orchestrated result had no matching tool_call; pushing in completion order as fallback"
                );
                // Fallback: push into the first empty slot.
                if let Some(slot) = ordered.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(tool_result);
                }
            }
        }

        // A5: total batch timeout. Per-call timeout is set per-spawn
        // above (60 s); the total here caps the whole batch at 5 min
        // so a pathological case (every tool near-completion-but-
        // never-returns) still terminates.
        let total_timeout = std::time::Duration::from_secs(300);
        let deadline = std::time::Instant::now() + total_timeout;

        while ordered.iter().any(Option::is_none) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                // Batch-wide timeout: fill every still-pending slot
                // with a synthetic error envelope.
                for (idx, slot) in ordered.iter_mut().enumerate() {
                    if slot.is_none() {
                        let tc = &tool_calls[idx];
                        let err = ToolResult::err(
                            &tc.id,
                            format!(
                                "batch-wide timeout after {}s: tool '{}' did not return in time",
                                total_timeout.as_secs(),
                                tc.function.name,
                            ),
                        );
                        tool_errors.push(ToolErrorRecord {
                            tool_name: tc.function.name.clone(),
                            error: err.content.clone(),
                            turn,
                        });
                        *slot = Some(err);
                    }
                }
                break;
            }
            match tokio::time::timeout(remaining, join_set.join_next()).await {
                Ok(Some(Ok(tool_result))) => {
                    if tool_result.is_error {
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    if let Some(&idx) = index_by_id.get(tool_result.tool_call_id.as_str()) {
                        ordered[idx] = Some(tool_result);
                    } else {
                        tracing::error!(
                            tool_call_id = %tool_result.tool_call_id,
                            "JoinSet result had no matching tool_call; pushing as fallback"
                        );
                        if let Some(slot) = ordered.iter_mut().find(|s| s.is_none()) {
                            *slot = Some(tool_result);
                        }
                    }
                }
                Ok(Some(Err(join_err))) => {
                    // We don't track the task_id → tool_call_id
                    // mapping here (the JoinSet path doesn't need it
                    // for the normal case), so we can't pinpoint
                    // which call panicked. Place the error in the
                    // first empty slot and log the dead task id.
                    // If no empty slot exists (everything else
                    // already returned), the outer loop will exit on
                    // its own and the missing_slot fallback will
                    // catch the gap.
                    let dead_id = join_err.id();
                    if let Some((idx, slot)) = ordered
                        .iter_mut()
                        .enumerate()
                        .find(|(_, s)| s.is_none())
                    {
                        let tc = &tool_calls[idx];
                        tool_errors.push(ToolErrorRecord {
                            tool_name: tc.function.name.clone(),
                            error: format!("Task join error: {join_err}"),
                            turn,
                        });
                        *slot = Some(ToolResult::err(
                            &tc.id,
                            format!("Task join error (id={dead_id}): {join_err}"),
                        ));
                    } else {
                        tracing::error!(
                            "execute_tool_calls: JoinSet join error with no empty slot: {join_err}"
                        );
                    }
                }
                Ok(None) => break, // JoinSet drained before we filled every slot
                Err(_elapsed) => continue, // outer loop will catch zero remaining
            }
        }

        // ★ if any tool call mutated a memory store, bump the
        // context epoch so the next `resolve_initial_system_prompt` (on the
        // next user message) force-rebuilds the Context layer from disk
        // instead of reusing the frozen `stored_system_prompt`. Without
        // this, `memory` tool edits (add/replace/remove) would be invisible
        // to the LLM until the next session restart.
        if tool_calls.iter().any(is_memory_store_mutation) {
            self.bump_context_epoch();
            tracing::debug!(
                target: "hermix.context_epoch",
                "memory store mutated by tool call; bumped context_epoch for next prompt rebuild"
            );
        }

        ordered
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    // Final safety net: every slot should be filled by
                    // the loops above, but if not (e.g. an inner panic
                    // and a rerouted fallback), synthesise an error
                    // rather than panic the agent.
                    ToolResult::err(
                        "missing",
                        "execute_tool_calls: no result produced for this slot (internal error)".to_string(),
                    )
                })
            })
            // P8-T3 wire: redact secret-shaped strings (API keys, IPs,
            // emails) from every tool result *before* the LLM sees it.
            // Same strategy as the legacy P8-T3 helper; does not fail
            // the call — just rewrites the content. The `_redactions`
            // vec is dropped on the floor (an HTTP callback would let
            // the trace panel display "redacted 3 spans").
            .map(|r| {
                let (sanitised, _redactions) =
                    crate::context_engine::redact_tool_result(&r.content);
                ToolResult {
                    tool_call_id: r.tool_call_id,
                    content: sanitised,
                    is_error: r.is_error,
                    denial_kind: None,
                }
            })
            .collect()
    }
}

/// Detect whether a tool call mutates a memory store whose change must be
/// reflected in the next system-prompt rebuild.
/// /// The `memory` tool rewrites `memories/MEMORY.md` / `memories/USER.md` on
/// disk for `action in {add, replace, remove}` (the frozen context snapshot
/// in the system prompt is sourced from those files). Bumping the context
/// epoch after such a call makes `build_context_layer` (`PromptCache`-keyed
/// by epoch) re-read those files on the next user message, so in-session
/// memory edits take effect immediately instead of only after a session
/// restart. `read` (and any non-`memory` tool) is non-mutating. (2026-07-23)
fn is_memory_store_mutation(tc: &ToolCall) -> bool {
    if tc.function.name != "memory" {
        return false;
    }
    let args: Value = match serde_json::from_str(&tc.function.arguments) {
        Ok(v) => v,
        Err(_) => return false,
    };
    matches!(
        args.get("action").and_then(|a| a.as_str()),
        Some("add") | Some("replace") | Some("remove")
    )
}
