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
use std::sync::Arc;

use hermix_core::{ToolCall, ToolErrorRecord, ToolResult};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::agent_loop::AgentLoop;

/// Maximum number of tool calls that execute concurrently in the JoinSet.
/// LLM can emit N tool_calls per turn; without a cap, N=10 web_search calls
/// would fire 10 simultaneous HTTP requests and trigger rate limits.
/// This semaphore limits concurrency to 5 — enough for parallelism, low
/// enough to avoid overwhelming external services.
const MAX_CONCURRENT_TOOLS: usize = 5;

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
        // Enforce max_tool_calls_per_turn limit.
        //
        // Split out of the async body into a non-async helper because
        // re-entering an `async fn` recursively needs boxing (E0733).
        if let Some(limit) = self.limit_capped_slice(tool_calls) {
            let (run_these, skipped_envelopes) = split_at_limit(tool_calls, limit);
            let mut results = self
                .execute_bounded(run_these, turn, parent_budget_remaining_usd, tool_errors)
                .await;
            results.extend(skipped_envelopes);
            return results;
        }
        self.execute_bounded(tool_calls, turn, parent_budget_remaining_usd, tool_errors)
            .await
    }

    /// `Some(limit)` when the batch exceeds `max_tool_calls_per_turn`,
    /// `None` when it fits and should run whole.
    fn limit_capped_slice(&self, tool_calls: &[ToolCall]) -> Option<usize> {
        let max = self.config.max_tool_calls_per_turn;
        if max > 0 && tool_calls.len() > max as usize {
            Some(max as usize)
        } else {
            None
        }
    }

    /// Split into (calls that run, envelopes for the ones that must not).
    ///
    /// Every call gets an answer: the first `limit` execute, the excess
    /// get a "not started" envelope. The old code returned envelopes for
    /// the excess ONLY, leaving the first `limit` calls with no result
    /// while the assistant message still listed all of them — a dangling
    /// `tool_calls` that providers reject ("tool call result does not
    /// follow tool call"; Anthropic has no repair pass).
    ///
    /// A free function, not a method: the returned slice borrows from
    /// `tool_calls` and must not be tied to `&self`'s lifetime.
    ///
    async fn execute_bounded(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        if tool_calls.is_empty() {
            return Vec::new();
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
        // Maps a spawned task's id back to the tool_call_id it is
        // executing. Without it, a JoinError (panic / cancellation)
        // could only be attributed to "the first empty slot", which
        // mislabels the tool in `tool_errors` and leaves the real
        // call's slot to be filled by a synthetic timeout error.
        let mut task_index: HashMap<tokio::task::Id, String> = HashMap::new();
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();
        // Semaphore limits concurrent tool execution in the JoinSet.
        // Each spawned task acquires a permit before running the handler;
        // the permit is released when the task completes. This prevents
        // a burst of N tool calls from overwhelming external services.
        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TOOLS));

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
                //push a focus-stack frame BEFORE delegating.
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
                //+: if the caller passed an explicit
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
                    if let Some(bad) = crate::sub_agent_safety::contains_blocked_tool(&names) {
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
                    //Parse role from args; default to General.
                    role: parsed
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(|s| match s {
                            "explore" => Some(crate::sub_agent_orchestrator::SubAgentRole::Explore),
                            "plan" => Some(crate::sub_agent_orchestrator::SubAgentRole::Plan),
                            "general" => Some(crate::sub_agent_orchestrator::SubAgentRole::General),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    //Parse parent_task_id from args if present.
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
                // Orchestrator spawns execute_inner on its own tokio task
                // (breaking async recursion) and WAITS for the child to
                // finish — synchronous from the LLM's perspective. The
                // result string is returned directly; no polling needed.
                let output = orch.execute_async(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
            }
        }

        //P2.4 : run remaining interactive tools (currently
        // just `clarify`) sequentially in the caller's task BEFORE
        // launching the JoinSet. This blocks the parallel work until the
        // user has answered the clarifying question — otherwise we'd
        // race the user response and accumulate stale tool results in
        // the LLM context. delegate_task is already handled above by
        // the orchestrator path and is filtered out of this loop.
        for tc in tool_calls {
            if tc.function.name == "delegate_task" {
                continue;
            }
            // 两条串行判据，命中任一即不进 JoinSet：
            //  · interactive —— 需要用户当场回答（clarify）
            //  · 会改状态   —— 并行跑会互相踩（write_file / patch / terminal）
            // 第二条是后加的：调度器此前从不读 `RiskLevel`，于是同一轮里
            // 的写与读会被同时下发。判定见 agent_helpers 的文档串。
            if !crate::agent_helpers::is_interactive_tool(&tc.function.name)
                && !crate::agent_helpers::is_state_mutating_tool(
                    self.tool_registry.as_ref(),
                    &tc.function.name,
                )
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
            //P2.4 : interactive tools run sequentially in
            // the orchestrator pass above (delegate_task) or — for clarify
            // — never enter this JoinSet at all. We extract and handle
            // them right after the orchestrator loop. Skipping them here
            // keeps the JoinSet reserved for genuinely-independent work.
            if crate::agent_helpers::is_interactive_tool(&tc.function.name)
                && !matches!(tc.function.name.as_str(), "delegate_task")
            {
                continue;
            }
            // 上面那圈已经串行跑完了会改状态的工具（见本轮的两条判据），
            // 这里再排一次是为了不让它们二次进入 JoinSet。
            if !crate::agent_helpers::is_interactive_tool(&tc.function.name)
                && crate::agent_helpers::is_state_mutating_tool(
                    self.tool_registry.as_ref(),
                    &tc.function.name,
                )
            {
                continue;
            }
            //push a focus-stack frame for non-orchestrated
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

            // Per-call budget is declared per tool (see
            // hermix_tools::dispatch::per_tool_budget) — the old flat 60s
            // cut off legitimate foreground builds/tests and contradicted
            // terminal's own 120s inner timeout.
            let per_call_timeout = hermix_tools::dispatch::per_tool_budget(&tool_name);
            let sem = semaphore.clone();
            // Keep an owned copy for the task index below — the original
            // is moved into the spawned future.
            let index_call_id = tool_call_id.clone();
            let spawned = join_set.spawn(async move {
                // Acquire a concurrency permit before executing the handler.
                // This blocks (await) until a slot is free, throttling
                // the batch to MAX_CONCURRENT_TOOLS simultaneous calls.
                let _permit = sem.acquire().await.expect("semaphore closed unexpectedly");
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
                // that never arrives, a `web_extract` on a wedged
                // server), this returns an error envelope instead of
                // wedging the whole batch. Budget is per-tool declared.
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
                            "per-call timeout after {}s: tool '{}' did not return in time. \
                             [请勿重试] 该工具响应超时, 重复调用大概率仍会超时, 请改用其他方式 \
                             (如换工具/换参数/换数据源) 或告知用户该操作暂不可用。",
                            per_call_timeout.as_secs(),
                            tool_name,
                        ),
                    ),
                }
            });
            // task_id → tool_call_id, so a panicking task is attributed
            // to the call that actually panicked instead of to whichever
            // slot happened to be empty first.
            task_index.insert(spawned.id(), index_call_id);
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

        // A5: total batch timeout — sum of per-tool budgets + slack,
        // capped at 3600s (see hermix_tools::dispatch::batch_budget).
        // The old flat 300s could kill a legitimate batch of long
        // foreground calls before any of them finished.
        let total_timeout = hermix_tools::dispatch::batch_budget(
            &tool_calls
                .iter()
                .map(|tc| tc.function.name.clone())
                .collect::<Vec<_>>(),
        );
        let deadline = std::time::Instant::now() + total_timeout;

        while ordered.iter().any(Option::is_none) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                // Batch-wide timeout. Before giving up on the in-flight
                // tasks, give them a short bounded grace period and drain
                // whatever lands — dropping the `JoinSet` here would abort
                // them mid-await, and an already-dispatched write (a
                // `tokio::fs` op on the blocking pool, a `terminal`
                // command that has already started) completes anyway.
                // Draining lets those land in THIS turn's results instead
                // of being silently orphaned with no observer.
                const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
                let grace_deadline = std::time::Instant::now() + DRAIN_GRACE;
                let mut drained = 0usize;
                while std::time::Instant::now() < grace_deadline {
                    let left = grace_deadline.saturating_duration_since(std::time::Instant::now());
                    match tokio::time::timeout(left, join_set.join_next()).await {
                        Ok(Some(Ok(res))) => {
                            drained += 1;
                            if let Some(&idx) = index_by_id.get(res.tool_call_id.as_str()) {
                                if ordered[idx].is_none() {
                                    if res.is_error {
                                        if let Some(tc) =
                                            tool_calls.iter().find(|tc| tc.id == res.tool_call_id)
                                        {
                                            tool_errors.push(ToolErrorRecord {
                                                tool_name: tc.function.name.clone(),
                                                error: res.content.clone(),
                                                turn,
                                            });
                                        }
                                    }
                                    ordered[idx] = Some(res);
                                }
                            }
                        }
                        // A panicked/cancelled task during the drain: record
                        // it against its real slot and move on.
                        Ok(Some(Err(join_err))) => {
                            let dead = join_err.id();
                            if let Some(call_id) = task_index.remove(&dead) {
                                if let Some(&idx) = index_by_id.get(call_id.as_str()) {
                                    if ordered[idx].is_none() {
                                        let tc = &tool_calls[idx];
                                        ordered[idx] = Some(ToolResult::err(
                                            &tc.id,
                                            format!("Task join error during drain: {join_err}"),
                                        ));
                                    }
                                }
                            }
                        }
                        // Drained fully, or grace expired.
                        Ok(None) | Err(_) => break,
                    }
                }
                if drained > 0 {
                    tracing::warn!(
                        drained,
                        grace_secs = DRAIN_GRACE.as_secs(),
                        "batch timeout: drained in-flight tool results before synthesizing errors"
                    );
                }
                // Fill every still-pending slot with a synthetic error
                // envelope. Anything still `None` here genuinely never
                // produced a result, so the message must warn the model
                // that the tool may have had side effects.
                for (idx, slot) in ordered.iter_mut().enumerate() {
                    if slot.is_none() {
                        let tc = &tool_calls[idx];
                        let err = ToolResult::err(
                            &tc.id,
                            format!(
                                "batch-wide timeout after {}s: tool '{}' did not return in time. \
                                 [请勿盲目重试] 该工具可能已部分执行（副作用可能已发生），\
                                 重试前请先确认其影响；建议改用其他方式或告知用户该操作暂不可用。",
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
                    // Attribute the failure to the call that actually
                    // panicked. The previous fallback ("first empty
                    // slot") mislabeled `tool_errors.tool_name` and left
                    // the real slot to be filled later by a synthetic
                    // batch-timeout error, hiding the true culprit.
                    let dead_id = join_err.id();
                    let Some(dead_call_id) = task_index.remove(&dead_id) else {
                        tracing::error!(
                            "execute_tool_calls: JoinSet join error with unknown task id: {join_err}"
                        );
                        continue;
                    };
                    let Some(&idx) = index_by_id.get(dead_call_id.as_str()) else {
                        tracing::error!(
                            call_id = %dead_call_id,
                            "execute_tool_calls: panicked task has no matching tool_call"
                        );
                        continue;
                    };
                    let tc = &tool_calls[idx];
                    tracing::error!(
                        tool = %tc.function.name,
                        call_id = %tc.id,
                        "execute_tool_calls: tool task panicked or was cancelled"
                    );
                    tool_errors.push(ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: format!("Task join error: {join_err}"),
                        turn,
                    });
                    ordered[idx] = Some(ToolResult::err(
                        &tc.id,
                        format!("Task join error: {join_err}"),
                    ));
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
            .enumerate()
            .map(|(idx, slot)| {
                slot.unwrap_or_else(|| {
                    // Final safety net: every slot should be filled by
                    // the loops above, but if not (e.g. the JoinSet
                    // draining early via `Ok(None) => break`), synthesise
                    // an error rather than panic the agent.
                    //
                    // The id MUST be this slot's own `tool_calls[idx].id`.
                    // The previous hardcoded `"missing"` produced a tool
                    // message that pairs with nothing, which is exactly the
                    // dangling-tool_call state the provider rejects
                    // ("tool call result does not follow tool call") — the
                    // "safety net" was itself the bug.
                    let call_id = tool_calls
                        .get(idx)
                        .map(|tc| tc.id.as_str())
                        .unwrap_or("missing");
                    tracing::error!(
                        tool_call_id = %call_id,
                        "execute_tool_calls: slot left unfilled; synthesising error result"
                    );
                    ToolResult::err(
                        call_id,
                        "execute_tool_calls: this tool call produced no result (internal error). \
                         [请勿盲目重试] 它可能已部分执行（副作用可能已发生）。"
                            .to_string(),
                    )
                })
            })
            //wire: redact secret-shaped strings (API keys, IPs,
            // emails) from every tool result *before* the LLM sees it.
            //Same strategy as the legacy helper; does not fail
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

/// Split a batch into (calls that run, envelopes for the ones that must
/// not).
///
/// Every call gets an answer: the first `limit` execute, the excess get a
/// "not started" envelope. The old code returned envelopes for the excess
/// ONLY, leaving the first `limit` calls with no result while the
/// assistant message still listed all of them — a dangling `tool_calls`
/// that providers reject ("tool call result does not follow tool call";
/// `AnthropicProvider` has no repair pass for it).
///
/// A free function rather than a method so the returned slice borrows
/// from `tool_calls` instead of from `&self`.
fn split_at_limit(tool_calls: &[ToolCall], limit: usize) -> (&[ToolCall], Vec<ToolResult>) {
    tracing::warn!(
        calls = tool_calls.len(),
        limit,
        skipped = tool_calls.len() - limit,
        "Tool call limit exceeded; running the first {} and answering the rest as not-started",
        limit
    );
    let skipped: Vec<ToolResult> = tool_calls
        .iter()
        .skip(limit)
        .map(|tc| {
            ToolResult::err(
                &tc.id,
                format!(
                    "Tool call limit reached ({}/{}). This tool call was not started. \
                     [请勿盲目重试] 它没有执行过；若确需执行，请减少同一轮的工具调用数量。",
                    limit,
                    tool_calls.len()
                ),
            )
        })
        .collect();
    (&tool_calls[..limit], skipped)
}

/// Detect whether a tool call mutates a memory store whose change must be
/// reflected in the next system-prompt rebuild.
/// The `memory` tool rewrites `memories/MEMORY.md` / `memories/USER.md` on
/// disk for `action in {add, replace, remove}` (the frozen context snapshot
/// in the system prompt is sourced from those files). Bumping the context
/// epoch after such a call makes `build_context_layer` (`PromptCache`-keyed
/// by epoch) re-read those files on the next user message, so in-session
/// memory edits take effect immediately instead of only after a session
/// restart. `read` (and any non-`memory` tool) is non-mutating.
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

#[cfg(test)]
mod split_limit_tests {
    use super::*;
    use hermix_core::FunctionCall;

    fn calls(n: usize) -> Vec<ToolCall> {
        (0..n)
            .map(|i| ToolCall {
                id: format!("call-{i}"),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: "{}".to_string(),
                },
            })
            .collect()
    }

    #[test]
    fn every_call_gets_an_answer() {
        // Regression: the old code returned envelopes only for the
        // skipped calls, so the first `limit` calls had no result while
        // the assistant message still listed all of them — a dangling
        // tool_calls that AnthropicProvider (no repair pass) rejects.
        let batch = calls(5);
        let (run, skipped) = split_at_limit(&batch, 2);
        assert_eq!(run.len(), 2, "the first `limit` calls run");
        assert_eq!(skipped.len(), 3, "the excess get an envelope");
        let answered: Vec<&str> = run
            .iter()
            .map(|tc| tc.id.as_str())
            .chain(skipped.iter().map(|r| r.tool_call_id.as_str()))
            .collect();
        assert_eq!(answered.len(), 5);
        for tc in &batch {
            assert!(
                answered.contains(&tc.id.as_str()),
                "call {} got no result at all",
                tc.id
            );
        }
    }

    #[test]
    fn skipped_envelopes_carry_their_own_call_ids() {
        let batch = calls(3);
        let (_, skipped) = split_at_limit(&batch, 1);
        assert_eq!(skipped[0].tool_call_id, "call-1");
        assert_eq!(skipped[1].tool_call_id, "call-2");
        assert!(
            skipped[0].content.contains("not started"),
            "the model must be told the call never ran, got: {}",
            skipped[0].content
        );
    }
}
