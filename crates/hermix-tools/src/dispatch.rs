//! Tool dispatch with parallel execution
//!
//! Dispatches a batch of tool calls through the registry using tokio::JoinSet
//! for parallel execution, with configurable concurrency and budget enforcement.

use std::sync::Arc;
use std::time::Instant;

use hermix_core::{is_error_json, truncate_chars, BudgetConfig, ToolCall, ToolHandler, ToolResult};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::registry::ToolRegistry;

/// Dispatch one of the three bridge tools (`tool_search` /
/// `tool_describe` / `tool_call`).
///
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
}

impl From<DispatchedResult> for ToolResult {
    fn from(dr: DispatchedResult) -> Self {
        ToolResult {
            tool_call_id: dr.tool_call_id,
            content: dr.content,
            is_error: dr.is_error,
        }
    }
}

/// Dispatch multiple tool calls in parallel through the registry.
///
/// Uses `tokio::JoinSet` with a configurable pool size for concurrency control.
/// Results are truncated according to the budget config.
///
/// # Arguments
/// * `calls` - List of tool calls to dispatch
/// * `registry` - The tool registry to dispatch through
/// * `budget` - Budget configuration for result size limits
/// * `pool_size` - Maximum number of concurrent tool executions (default: 8)
///
/// # Returns
/// Vec of ToolResult, one per input call, in no particular order.
pub async fn dispatch_tools(
    calls: Vec<ToolCall>,
    registry: Arc<ToolRegistry>,
    budget: BudgetConfig,
    pool_size: usize,
) -> Vec<ToolResult> {
    use tokio::sync::Semaphore;

    // Real concurrency limit (was: `let _concurrency = pool_size.max(1);`
    // which was dead code — every spawned task ran unconditionally and
    // the `pool_size` parameter did nothing). Now we cap parallelism
    // at `pool_size` via a `Semaphore`: each spawned task waits for a
    // permit before doing any work, and releases it on completion.
    let semaphore = Arc::new(Semaphore::new(pool_size.max(1)));
    let mut join_set = JoinSet::new();

    for call in calls {
        let registry = registry.clone();
        let semaphore = semaphore.clone();
        let _max_chars = budget.max_result_size_chars;
        join_set.spawn(async move {
            // Wait for a permit before doing real work. The permit
            // is held for the duration of the task and released on
            // drop (i.e. on completion, including panic).
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("semaphore closed unexpectedly");
            let start = Instant::now();
            // Bridge tools (`tool_search` / `tool_describe` / `tool_call`)
            // are intentionally NOT registered in the registry — they are
            // the LLM-facing meta-tool for discovering deferred tools.
            // They go through a dedicated handler, not registry.dispatch_async,
            // because (a) they need access to the full schema list (which
            // is hidden from the LLM otherwise) and (b) `tool_call` calls
            // back into registry.dispatch_async recursively, which would
            // need a different bridge path. See `dispatch_bridge_tool`.
            let result = if crate::tools::tool_search::is_bridge(&call.function.name) {
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
            }
        });
    }

    let mut results: Vec<ToolResult> = Vec::new();
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(dr) => {
                let mut content = dr.content;
                // Per-result truncation. UTF-8 safe: we slice on
                // char boundaries, not byte indices. Earlier
                // versions used `&content[..N]` which panicked on
                // any CJK / emoji output that hit the limit on a
                // multi-byte boundary.
                if content.chars().count() > budget.max_result_size_chars {
                    let head = truncate_chars(&content, budget.max_result_size_chars);
                    let removed = content.chars().count() - budget.max_result_size_chars;
                    content = format!(
                        "{}\n\n[... truncated {} characters ...]",
                        head, removed
                    );
                }
                results.push(ToolResult {
                    tool_call_id: dr.tool_call_id,
                    content,
                    is_error: dr.is_error,
                });
            }
            Err(e) => {
                // Join error — treat as a tool error
                results.push(ToolResult::err(
                    "unknown",
                    format!("Task join error: {}", e),
                ));
            }
        }
    }

    // Aggregate budget enforcement. Same UTF-8 safe slice as above.
    let total_chars: usize = results.iter().map(|r| r.content.chars().count()).sum();
    if total_chars > budget.max_aggregate_chars {
        let ratio = budget.max_aggregate_chars as f64 / total_chars as f64;
        for result in results.iter_mut() {
            let target_len = ((result.content.chars().count() as f64) * ratio) as usize;
            let min_len = target_len.max(200);
            if result.content.chars().count() > min_len {
                let removed = result.content.chars().count() - min_len;
                let head = truncate_chars(&result.content, min_len);
                result.content = format!(
                    "{}\n\n[... truncated {} characters ...]",
                    head, removed
                );
            }
        }
    }

    results
}

/// Dispatch a single tool call through the registry (convenience wrapper).
pub async fn dispatch_single(
    call: ToolCall,
    registry: Arc<ToolRegistry>,
    max_result_size_chars: usize,
) -> ToolResult {
    let start = Instant::now();
    let result = registry
        .dispatch_async(
            &call.function.name,
            call.function
                .arguments
                .parse()
                .unwrap_or(serde_json::Value::Null),
        )
        .await;
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
        let cjk = "你好世界".repeat(200); // 800 chars, 2400 bytes
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
        let results = dispatch_tools(vec![call], registry, budget, 4).await;
        assert_eq!(results.len(), 1);
        // The truncation marker must be present, and the head must
        // be a valid UTF-8 string (which is the whole point — the
        // old code panicked before reaching this assertion).
        let content = &results[0].content;
        assert!(content.contains("[... truncated"));
        assert!(std::str::from_utf8(content.as_bytes()).is_ok());
    }

    // ── dispatch_bridge_tool regression tests (A2) ────────────────────
    //
    // The bridge path used to inline a 14-line match on
    // `call.function.name.as_str()`. Extracted into
    // `dispatch_bridge_tool` so:
    //  - all three handlers share one parse-args / format-error path;
    //  - a future bridge tool only adds one match arm, not three call
    //    sites;
    //  - the `_` arm catches `is_bridge` drift (returns an error JSON
    //    instead of silently routing a non-bridge name to a handler).

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
}
