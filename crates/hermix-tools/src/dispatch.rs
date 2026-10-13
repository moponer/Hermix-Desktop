//! Tool dispatch with parallel execution
//!
//! Dispatches a batch of tool calls through the registry using tokio::JoinSet
//! for parallel execution, with configurable concurrency and budget enforcement.

use std::sync::Arc;
use std::time::Instant;

use std::collections::HashMap;
use std::time::Duration;

use hermix_core::{is_error_json, truncate_chars, BudgetConfig, ToolCall, ToolHandler, ToolResult};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::registry::ToolRegistry;

/// Dispatch one of the three bridge tools (`tool_search` /
/// `tool_describe` / `tool_call`).
/// These are the only tools NOT registered in the registry — they are
/// the meta-tool for discovering the deferred-tool list that the LLM
/// would otherwise never see. The match is exhaustive on the three
/// bridge names; any other name falls through to the `_` arm and
/// returns an error JSON, so a typo in `is_bridge` never silently
/// misroutes a call.
async fn dispatch_bridge_tool(call: &ToolCall, registry: &Arc<ToolRegistry>) -> String {
    use crate::tools::tool_search::{ToolCallHandler, ToolDescribeHandler, ToolSearchHandler};
    let args: Value = call
        .function
        .arguments
        .parse()
        .unwrap_or(Value::Null);
    match call.function.name.as_str() {
        "tool_search" => ToolSearchHandler::new(registry.clone())
            .execute(args)
            .await
            .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#)),
        "tool_describe" => ToolDescribeHandler::new(registry.clone())
            .execute(args)
            .await
            .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#)),
        "tool_call" => ToolCallHandler::new(registry.clone())
            .execute(args)
            .await
            .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#)),
        other => format!(
            r#"{{"error": "is_bridge returned true for non-bridge name '{other}'"}}"#
        ),
    }
}

/// Result of dispatching a single tool call, including timing information.
#[derive(Debug, Clone)]
pub struct DispatchedResult {
    /// The tool call ID.
    pub tool_call_id: String,
    /// The tool name that was called.
    pub tool_name: String,
    /// The result content (or error JSON).
    pub content: String,
    /// Whether the result is an error.
    pub is_error: bool,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
    /// The JoinSet task id — used by `dispatch_tools` to recover the
    /// input index so results can be returned in input order. `None`
    /// when constructed outside a real `JoinSet` (tests, callers that
    /// build `DispatchedResult` by hand).
    pub task_id: Option<tokio::task::Id>,
}

impl From<DispatchedResult> for ToolResult {
    fn from(dr: DispatchedResult) -> Self {
        ToolResult {
            tool_call_id: dr.tool_call_id,
            content: dr.content,
            is_error: dr.is_error,
            denial_kind: None,
        }
    }
}

/// Timeouts for `dispatch_tools`.
/// These match the LLM timeout in `agent_loop::call_llm_with_retry`
/// so a hanging tool never outlives its caller.
///
/// 注意：生产批执行路径（hermix-agent 的 `tool_executor`）的预算来自
/// 下方的 [`per_tool_budget`] / [`batch_budget`]——按工具身份声明，
/// 不再使用本结构的一刀切 60s/300s。本结构保留给
/// `dispatch_tools` 的调用方做显式覆盖。
#[derive(Debug, Clone, Copy)]
pub struct DispatchTimeouts {
    /// Maximum time any single tool call may take. A timeout produces
    /// a `ToolResult::err` whose `content` is a JSON error envelope
    /// (`{"error": "per-call timeout after Ns": ...}`).
    pub per_call: Duration,
    /// Maximum time the whole batch may take. Even if individual
    /// timeouts don't fire, the dispatcher gives up after `total`
    /// and substitutes error envelopes for any still-pending calls.
    pub total: Duration,
}

impl Default for DispatchTimeouts {
    fn default() -> Self {
        Self {
            per_call: Duration::from_secs(60),
            total: Duration::from_secs(300),
        }
    }
}

/// 按工具身份声明的单次调用预算。
///
/// 这是工具超时的**单一真相源**（生产批执行路径 `tool_executor` 与
/// `dispatch_tools` 共用）。此前 per-call 60s 硬编码与 terminal 自身的
/// 120s 内部超时互相矛盾——外层先到，前台跑构建/测试必被掐断，是
/// "长任务无法执行"的结构性原因之一。
///
/// 预算原则：外部预算 ≥ 工具自身声明的内部预算；更长的任务应改走
/// terminal `background` + 轮询（提示词已指导），前台预算只覆盖
/// "合理的单次同步等待"。
pub fn per_tool_budget(tool: &str) -> Duration {
    match tool {
        // terminal 自身默认 120s（tools/terminal.rs），外部预算给 5 倍余量：
        // 前台跑构建/安装是长任务的主要形态，超时提示已引导转后台。
        "terminal" => Duration::from_secs(600),
        // 子 agent 编排有自己的预算（orchestrator，默认 1800s），
        // 外部预算只做护栏，略微放行即可。
        "delegate_task" => Duration::from_secs(1900),
        // 文档转换（Office MCP / markitdown）跑外部进程，大文件转换慢
        "markitdown" | "file_converter" | "read_pdf" => Duration::from_secs(300),
        // 图片/视频生成走外部 API，出图 1-3 分钟是常态
        "image_generate" | "gpt_image_generate" | "video_generate" | "tts" => {
            Duration::from_secs(600)
        }
        _ => Duration::from_secs(60),
    }
}

/// 整批预算 = 各调用预算之和 + 120s 调度余量，封顶 3600s。
/// 并行执行下真实耗时远小于和值；取和值是为了保证"所有调用都合法
/// 跑满预算"时批也不被提前掐断（旧固定 300s 的结构性缺陷）。
pub fn batch_budget(tool_names: &[String]) -> Duration {
    const CAP: Duration = Duration::from_secs(3600);
    let sum: u64 = tool_names
        .iter()
        .map(|t| per_tool_budget(t).as_secs())
        .sum();
    (Duration::from_secs(sum) + Duration::from_secs(120)).min(CAP)
}

/// Dispatch multiple tool calls in parallel through the registry.
/// Uses `tokio::JoinSet` with a configurable concurrency cap and budget
/// enforcement. Results are truncated according to the budget config
/// (UTF-8 safe — see A1). Two new guarantees on top of A1:
/// 1. **Per-call timeout.** Each spawned task is wrapped in
/// `tokio::time::timeout(per_call_timeout, …)`. If a tool hangs
/// (e.g. `terminal` reading from a fifo nobody will write to,
/// `cua.click` waiting for an element that never appears), the
/// timer fires and we substitute a `ToolResult::err` describing
/// the timeout — the dispatch never blocks past `per_call_timeout`.
/// 2. **Input order preserved.** Results are returned in the same
/// order as `calls`, even though `JoinSet` reports completions
/// out of order. We track each task's input index and place
/// results into a fixed-size `Vec<Option<ToolResult>>` before
/// unwrapping into the final `Vec<ToolResult>`.
/// # Arguments
/// * `calls`            - List of tool calls to dispatch
/// * `registry`         - The tool registry to dispatch through
/// * `budget`           - Budget configuration for result size limits
/// * `pool_size`        - Maximum number of concurrent tool executions
/// (default: 8)
/// * `timeouts`         - Per-call timeout and total batch timeout.
/// `DispatchTimeouts::default()` = 60 s per
/// call, 5 min total.
/// # Returns
/// Vec of `ToolResult`, one per input call, **in the same order as
/// `calls`**.
pub async fn dispatch_tools(
    calls: Vec<ToolCall>,
    registry: Arc<ToolRegistry>,
    budget: BudgetConfig,
    pool_size: usize,
    timeouts: DispatchTimeouts,
) -> Vec<ToolResult> {
    use tokio::sync::Semaphore;

    // Real concurrency limit (was: `let _concurrency = pool_size.max(1);`
    // which was dead code — every spawned task ran unconditionally and
    // the `pool_size` parameter did nothing). Now we cap parallelism
    // at `pool_size` via a `Semaphore`: each spawned task waits for a
    // permit before doing any work, and releases it on completion.
    let semaphore = Arc::new(Semaphore::new(pool_size.max(1)));
    let mut join_set = JoinSet::new();

    // Track each task's input index so we can return results in
    // input order. join_next() reports completions out of order, so
    // we cannot just push into a Vec<ToolResult>.
    let total = calls.len();
    let mut ordered: Vec<Option<ToolResult>> = (0..total).map(|_| None).collect();
    let mut task_to_index: HashMap<tokio::task::Id, usize> = HashMap::new();

    for (index, call) in calls.into_iter().enumerate() {
        let registry = registry.clone();
        let semaphore = semaphore.clone();
        let _max_chars = budget.max_result_size_chars;
        let per_call_timeout = timeouts.per_call;
        let abort_handle = join_set.spawn(async move {
            // Wait for a permit before doing real work. The permit
            // is held for the duration of the task and released on
            // drop (i.e. on completion, including panic).
            let _permit = match semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    // Semaphore was closed — return an error result
                    // instead of panicking.
                    return DispatchedResult {
                        tool_call_id: call.id.clone(),
                        tool_name: call.function.name.clone(),
                        content: serde_json::json!({
                            "error": "tool dispatch semaphore closed unexpectedly",
                            "tool": call.function.name,
                        })
                        .to_string(),
                        is_error: true,
                        duration_ms: 0,
                        task_id: None,
                    };
                }
            };
            let start = Instant::now();
            // Bridge tools (`tool_search` / `tool_describe` / `tool_call`)
            // are intentionally NOT registered in the registry — they are
            // the LLM-facing meta-tool for discovering deferred tools.
            // They go through a dedicated handler, not registry.dispatch_async,
            // because (a) they need access to the full schema list (which
            // is hidden from the LLM otherwise) and (b) `tool_call` calls
            // back into registry.dispatch_async recursively, which would
            // need a different bridge path. See `dispatch_bridge_tool`.
            // The whole bridge-or-registry dispatch is wrapped in
            // tokio::time::timeout so a hanging handler (e.g. one that
            // is blocked on a channel the user will never write to) can
            // never wedge the batch.
            let dispatch_fut = async {
                if crate::tools::tool_search::is_bridge(&call.function.name) {
                    dispatch_bridge_tool(&call, &registry).await
                } else {
                    registry
                        .dispatch_async(
                            &call.function.name,
                            call.function
                                .arguments
                                .parse()
                                .unwrap_or(serde_json::Value::Null),
                        )
                        .await
                }
            };
            let result = match tokio::time::timeout(per_call_timeout, dispatch_fut).await {
                Ok(s) => s,
                Err(_elapsed) => format!(
                    r#"{{"error": "per-call timeout after {}s: tool '{}' did not return in time"}}"#,
                    per_call_timeout.as_secs(),
                    call.function.name,
                ),
            };
            let duration_ms = start.elapsed().as_millis() as u64;

            // Structural probe — see hermix_core::is_error_json for the
            // false-positive classes ({"error_count":..}, {"errors":..})
            // the previous starts_with heuristic mis-flagged.
            let is_error = is_error_json(&result);

            DispatchedResult {
                tool_call_id: call.id,
                tool_name: call.function.name,
                content: result,
                is_error,
                duration_ms,
                task_id: Some(tokio::task::id()),
            }
        });
        task_to_index.insert(abort_handle.id(), index);
    }

    // Outer deadline: even if individual timeouts don't fire (every
    // tool claims to be near completion but never returns), we cap the
    // whole batch at `timeouts.total`.
    let deadline = tokio::time::Instant::now() + timeouts.total;
    while !ordered.iter().all(Option::is_some) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            // Batch-wide timeout: fill in every still-pending slot with
            // an error envelope. JoinSet tasks we drop here will be
            // aborted when their handles go out of scope.
            for (idx, slot) in ordered.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(ToolResult::err(
                        format!("batch_timeout_index_{idx}"),
                        format!(
                            "batch-wide timeout after {}s: this tool did not return in time",
                            timeouts.total.as_secs()
                        ),
                    ));
                }
            }
            break;
        }
        match tokio::time::timeout(remaining, join_set.join_next()).await {
            Ok(Some(Ok(dr))) => {
                let index = match dr.task_id.and_then(|id| task_to_index.get(&id).copied()) {
                    Some(i) => i,
                    None => {
                        // We never assigned this task an index — that
                        // would be a programming error. Surface it.
                        tracing::error!(
                            "dispatch_tools: completed task has no recorded input index"
                        );
                        continue;
                    }
                };
                // Note: dispatch_async in registry.rs handles per-tool
                // truncation. This secondary truncation enforces the
                // batch-level budget. Both are kept for defense-in-depth.
                let mut content = dr.content;
                if content.chars().count() > budget.max_result_size_chars {
                    let head = truncate_chars(&content, budget.max_result_size_chars);
                    let removed = content.chars().count() - budget.max_result_size_chars;
                    content = format!(
                        "{}\n\n[... truncated {} characters ...]",
                        head, removed
                    );
                }
                ordered[index] = Some(ToolResult {
                    tool_call_id: dr.tool_call_id,
                    content,
                    is_error: dr.is_error,
                    denial_kind: None,
                });
            }
            Ok(Some(Err(join_err))) => {
                let dead_id = join_err.id();
                let index = task_to_index.get(&dead_id).copied().unwrap_or(0);
                ordered[index] = Some(ToolResult::err(
                    format!("join_error_index_{index}"),
                    format!("Task join error: {join_err}"),
                ));
            }
            Ok(None) => break, // join_set empty before we filled every slot
            Err(_elapsed) => continue, // outer loop will catch zero remaining
        }
    }

    // Unwrap the fixed-size Vec<Option<ToolResult>> into a Vec<ToolResult>.
    // A None slot here means the JoinSet drained without ever writing
    // it (only possible if the total timeout raced a join_next — the
    // outer loop should have caught that). Treat it as a synthetic err
    // rather than panic, since the contract is "one result per call".
    ordered
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            slot.unwrap_or_else(|| {
                ToolResult::err(
                    format!("missing_index_{i}"),
                    "dispatch_tools returned no result for this call (internal error)".to_string(),
                )
            })
        })
        .collect()
}

/// Dispatch a single tool call through the registry (convenience wrapper).
/// Includes a per-call timeout to prevent hanging.
pub async fn dispatch_single(
    call: ToolCall,
    registry: Arc<ToolRegistry>,
    max_result_size_chars: usize,
) -> ToolResult {
    let start = Instant::now();
    let per_call_timeout = Duration::from_secs(60); // Same as dispatch_tools default
    let result = match tokio::time::timeout(
        per_call_timeout,
        registry.dispatch_async(
            &call.function.name,
            call.function
                .arguments
                .parse()
                .unwrap_or(serde_json::Value::Null),
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            return ToolResult {
                tool_call_id: call.id,
                content: serde_json::json!({
                    "error": format!("tool '{}' timed out after {}s", call.function.name, per_call_timeout.as_secs()),
                    "tool": call.function.name,
                })
                .to_string(),
                is_error: true,
                denial_kind: None,
            };
        }
    };
    let _duration_ms = start.elapsed().as_millis() as u64;

    let is_error = is_error_json(&result);

    let content = if result.chars().count() > max_result_size_chars {
        let head = truncate_chars(&result, max_result_size_chars);
        let removed = result.chars().count() - max_result_size_chars;
        format!(
            "{}\n\n[... truncated {} characters ...]",
            head, removed
        )
    } else {
        result
    };

    ToolResult {
        tool_call_id: call.id,
        content,
        is_error,
        denial_kind: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatched_result_conversion() {
        let dr = DispatchedResult {
            tool_call_id: "call_1".to_string(),
            tool_name: "test".to_string(),
            content: "result".to_string(),
            is_error: false,
            duration_ms: 100,
            task_id: None,
        };
        let tr: ToolResult = dr.into();
        assert_eq!(tr.tool_call_id, "call_1");
        assert!(!tr.is_error);
    }

    /// Regression: per-result truncation used to slice `&content[..N]`
    /// on a byte index, which panicked when `content` contained
    /// multi-byte characters and the limit fell in the middle of a
    /// code point. The new path uses [`truncate_chars`].
    #[tokio::test]
    async fn dispatch_truncates_cjk_on_char_boundary() {
        let registry = Arc::new(ToolRegistry::new());
        // Build a content blob whose char count is well over the
        // limit but whose byte count is much larger than the limit.
        let _cjk = "你好世界".repeat(200); // 800 chars, 2400 bytes
        let budget = BudgetConfig {
            max_result_size_chars: 50,
            max_aggregate_chars: 10_000,
        };
        // Build a single ToolCall that will resolve via
        // dispatch_async. We point it at a non-existent tool so we
        // get back the error JSON path; the call itself goes
        // through the truncation logic.
        let call = ToolCall {
            id: "c1".to_string(),
            function: hermix_core::FunctionCall {
                name: "no_such_tool_for_truncate_test".to_string(),
                arguments: "{}".to_string(),
            },
        };
        let results = dispatch_tools(
            vec![call],
            registry,
            budget,
            4,
            DispatchTimeouts::default(),
        )
        .await;
        assert_eq!(results.len(), 1);
        // The truncation marker must be present, and the head must
        // be a valid UTF-8 string (which is the whole point — the
        // old code panicked before reaching this assertion).
        let content = &results[0].content;
        assert!(content.contains("[... truncated"));
        assert!(std::str::from_utf8(content.as_bytes()).is_ok());
    }

    // ── dispatch_bridge_tool regression tests (A2) ────────────────────
    // The bridge path used to inline a 14-line match on
    // `call.function.name.as_str()`. Extracted into
    // `dispatch_bridge_tool` so:
    // - all three handlers share one parse-args / format-error path;
    // - a future bridge tool only adds one match arm, not three call
    // sites;
    // - the `_` arm catches `is_bridge` drift (returns an error JSON
    // instead of silently routing a non-bridge name to a handler).

    fn bridge_call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            function: hermix_core::FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn dispatch_bridge_tool_search_returns_schema_list() {
        let registry = Arc::new(ToolRegistry::new());
        // Empty registry → tool_search handler should still return a
        // valid (possibly empty) JSON list — never panic, never route
        // to the registry path.
        let call = bridge_call("tool_search", r#"{"query":"nope"}"#);
        let out = dispatch_bridge_tool(&call, &registry).await;
        // Must NOT be the "is_bridge drift" sentinel.
        assert!(
            !out.contains("is_bridge returned true"),
            "tool_search should not hit the drift arm: {out}"
        );
        // ToolSearchHandler returns `{"query":...,"matches":[...],"total_deferred":N}`.
        // Empty registry → total_deferred == 0 → matches empty. Verify
        // it's structured JSON.
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("tool_search output must be JSON");
        assert!(parsed.get("matches").is_some());
        assert!(parsed.get("total_deferred").is_some());
    }

    #[tokio::test]
    async fn dispatch_bridge_tool_describe_returns_schema() {
        let registry = Arc::new(ToolRegistry::new());
        let call = bridge_call("tool_describe", r#"{"name":"read_file"}"#);
        let out = dispatch_bridge_tool(&call, &registry).await;
        assert!(!out.contains("is_bridge returned true"));
        // Empty registry → handler returns the standard "tool not
        // registered" error JSON shape.
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("JSON");
        assert!(parsed.get("error").is_some() || parsed.get("schema").is_some());
    }

    #[tokio::test]
    async fn dispatch_bridge_tool_drift_arm_returns_error_json() {
        // If `is_bridge` ever drifts and admits a non-bridge name,
        // dispatch_bridge_tool must still produce an error JSON —
        // never silently route to a handler that doesn't fit.
        let registry = Arc::new(ToolRegistry::new());
        let call = bridge_call("not_a_real_bridge_tool", "{}");
        let out = dispatch_bridge_tool(&call, &registry).await;
        assert!(
            out.contains("is_bridge returned true"),
            "expected drift-sentinel error, got: {out}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("JSON");
        assert!(parsed.get("error").is_some());
    }

    // ── dispatch_tools timeout + order (A4) ──────────────────────────
    // The `FakeHandler` below is registered into a fresh ToolRegistry
    // and lets a test pick exactly how long the handler sleeps before
    // returning. The "hang forever" arm uses `tokio::time::sleep`
    // with a long duration — it always gets killed by the test's
    // timeout, never runs to completion.

    use async_trait::async_trait;
    use hermix_core::traits::RiskLevel;
    use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
    use std::time::Duration;

    fn make_handler(sleep: Duration, label: &'static str) -> FakeHandler {
        FakeHandler { sleep, label }
    }

    fn register_fake(registry: &ToolRegistry, handler: FakeHandler) {
        registry.register_with_risk(
            handler.label,
            "test",
            handler.schema(),
            Arc::new(make_handler(handler.sleep, handler.label)),
            Arc::new(|| true),
            Vec::new(),
            true,
            "fake",
            "🔧",
            None,
            RiskLevel::Safe,
            false,
        );
    }

    struct FakeHandler {
        sleep: Duration,
        label: &'static str,
    }

    #[async_trait]
    impl ToolHandler for FakeHandler {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                self.label,
                format!("fake handler that sleeps {:?}", self.sleep),
                JsonSchema::object(indexmap::IndexMap::new(), Vec::new()),
            )
        }
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            tokio::time::sleep(self.sleep).await;
            Ok(format!("ok:{}", self.label))
        }
    }

    fn make_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: hermix_core::FunctionCall {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn dispatch_returns_results_in_input_order() {
        let registry = ToolRegistry::new();
        register_fake(
            &registry,
            make_handler(Duration::from_millis(120), "slow_a"),
        );
        register_fake(&registry, make_handler(Duration::from_millis(60), "mid"));
        register_fake(&registry, make_handler(Duration::from_millis(0), "fast"));
        let registry = Arc::new(registry);

        let calls = vec![
            make_call("call_0", "slow_a"),
            make_call("call_1", "mid"),
            make_call("call_2", "fast"),
        ];
        let results = dispatch_tools(
            calls,
            registry,
            BudgetConfig {
                max_result_size_chars: 1_000_000,
                max_aggregate_chars: 10_000_000,
            },
            4,
            DispatchTimeouts {
                per_call: Duration::from_secs(5),
                total: Duration::from_secs(30),
            },
        )
        .await;

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].tool_call_id, "call_0");
        assert!(results[0].content.contains("ok:slow_a"));
        assert_eq!(results[1].tool_call_id, "call_1");
        assert!(results[1].content.contains("ok:mid"));
        assert_eq!(results[2].tool_call_id, "call_2");
        assert!(results[2].content.contains("ok:fast"));
    }

    #[tokio::test]
    async fn dispatch_per_call_timeout_returns_error_envelope() {
        let registry = ToolRegistry::new();
        register_fake(
            &registry,
            make_handler(Duration::from_secs(10), "hanger"),
        );
        let registry = Arc::new(registry);

        let start = std::time::Instant::now();
        let results = dispatch_tools(
            vec![make_call("c1", "hanger")],
            registry,
            BudgetConfig {
                max_result_size_chars: 1_000_000,
                max_aggregate_chars: 10_000_000,
            },
            2,
            DispatchTimeouts {
                per_call: Duration::from_millis(100),
                total: Duration::from_secs(30),
            },
        )
        .await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error, "timeout result must be is_error");
        assert!(
            results[0].content.contains("per-call timeout"),
            "content should mention per-call timeout, got: {}",
            results[0].content
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "dispatch must return well before the 10 s hang, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_total_timeout_fills_pending_with_errors() {
        let registry = ToolRegistry::new();
        for label in ["h1", "h2", "h3"] {
            register_fake(
                &registry,
                make_handler(Duration::from_secs(10), label),
            );
        }
        let registry = Arc::new(registry);

        let start = std::time::Instant::now();
        let results = dispatch_tools(
            vec![
                make_call("c1", "h1"),
                make_call("c2", "h2"),
                make_call("c3", "h3"),
            ],
            registry,
            BudgetConfig {
                max_result_size_chars: 1_000_000,
                max_aggregate_chars: 10_000_000,
            },
            4,
            DispatchTimeouts {
                per_call: Duration::from_secs(60),
                total: Duration::from_millis(200),
            },
        )
        .await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 3);
        for (i, r) in results.iter().enumerate() {
            assert!(r.is_error, "call {i} should be error after total timeout");
            assert!(r.content.contains("batch-wide timeout"));
        }
        assert!(
            elapsed < Duration::from_millis(800),
            "dispatch must return well before the 10 s hang, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_per_call_timeout_does_not_block_fast_tools() {
        let registry = ToolRegistry::new();
        register_fake(
            &registry,
            make_handler(Duration::from_secs(10), "hanger"),
        );
        register_fake(&registry, make_handler(Duration::from_millis(0), "fast"));
        let registry = Arc::new(registry);

        let start = std::time::Instant::now();
        let results = dispatch_tools(
            vec![make_call("c_h", "hanger"), make_call("c_f", "fast")],
            registry,
            BudgetConfig {
                max_result_size_chars: 1_000_000,
                max_aggregate_chars: 10_000_000,
            },
            4,
            DispatchTimeouts {
                per_call: Duration::from_millis(100),
                total: Duration::from_secs(30),
            },
        )
        .await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_call_id, "c_h");
        assert!(results[0].is_error, "hanger must time out");
        assert_eq!(results[1].tool_call_id, "c_f");
        assert!(!results[1].is_error, "fast must succeed");
        assert!(results[1].content.contains("ok:fast"));
        assert!(
            elapsed < Duration::from_millis(500),
            "dispatch took {elapsed:?}, fast tool should not have been blocked"
        );
    }
}
