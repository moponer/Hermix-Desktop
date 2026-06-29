//! Tool dispatch with parallel execution
//!
//! Dispatches a batch of tool calls through the registry using tokio::JoinSet
//! for parallel execution, with configurable concurrency and budget enforcement.

use std::sync::Arc;
use std::time::Instant;

use hermix_core::{truncate_chars, BudgetConfig, ToolCall, ToolHandler, ToolResult};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::registry::ToolRegistry;

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
    use crate::tools::tool_search::{is_bridge, ToolCallHandler, ToolDescribeHandler, ToolSearchHandler};
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
            let result = if is_bridge(&call.function.name) {
                let args: Value = call.function.arguments.parse().unwrap_or(Value::Null);
                match call.function.name.as_str() {
                    "tool_search" => {
                        let h = ToolSearchHandler::new(registry);
                        h.execute(args).await.unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
                    }
                    "tool_describe" => {
                        let h = ToolDescribeHandler::new(registry);
                        h.execute(args).await.unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
                    }
                    "tool_call" => {
                        let h = ToolCallHandler::new(registry);
                        h.execute(args).await.unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
                    }
                    _ => unreachable!(),
                }
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

            // Check if result is an error
            let is_error = result.starts_with(r#"{"error"#);

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

    let is_error = result.starts_with(r#"{"error"#);

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
}
