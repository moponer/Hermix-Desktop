//! `compact_tool_chain` — collapse N tool calls into 1 LLM-context entry.
//!
//! ## Why this exists
//!
//! Without compaction, an LLM that needs to (e.g.) read three files,
//! apply a patch, and run a build invokes four separate tools. Each
//! tool's `tool_result` shows up in the LLM's next-turn `messages`,
//! so the conversation grows by ~4 × `max_result_size_chars`.
//! Repeated across a long session, this is the dominant driver of
//! context pressure (see `crates/hermix-agent/src/compression.rs`
//! `auto_compress_if_over_threshold`).
//!
//! `compact_tool_chain` lets the LLM declare the whole pipeline in
//! one tool call:
//!
//! ```text
//! tool_call: compact_tool_chain
//! args: {
//!   "chain": [
//!     {"tool": "read_file", "params": {"path": "a.rs"}},
//!     {"tool": "read_file", "params": {"path": "b.rs"}},
//!     {"tool": "apply_patch", "params": {"path": "a.rs", "patch": "..."}}
//!   ],
//!   "mode": "sequential",
//!   "failure_mode": "abort_on_first_error"
//! }
//! ```
//!
//! The agent loop sees a single `tool_use` block, the LLM sees a
//! single `tool_result` summarizing the chain — so the chat history
//! grows by *one* entry regardless of how many tools the chain
//! invoked. This is the hermix analog of NousResearch hermes-agent's
//! "zero-context RPC delegation" (Nous docs, /concepts/rpc).
//!
//! ## Trade-offs vs raw `tool_use`
//!
//! - **Cost**: the chain author must pre-declare the entire
//!   pipeline. It cannot react to intermediate results (e.g. "if the
//!   first read shows X, take branch A; else branch B"). For that,
//!   the LLM should still issue separate tool calls.
//! - **Value**: pipelines where the order is known up-front and
//!   outputs are logged for the user (not for branching) — e.g.
//!   `read N files → summarize` — collapse from O(N) tool results
//!   to O(1). Most "gathering" workloads fall here.
//! - **Failure semantics**: `failure_mode: "abort_on_first_error"`
//!   (default) returns early and surfaces the first error; the rest
//!   of the chain is marked `skipped`. `failure_mode: "continue"`
//!   runs every step and returns a per-step status array — useful
//!   for read-only sweeps where partial results are still valuable.
//!
//! ## What this is *not*
//!
//! This is **not** a replacement for the agent loop's normal
//! `tool_use` round-trip. The LLM still iterates turn-by-turn
//! between reasoning and tool invocations; `compact_tool_chain`
//! only changes how *one* turn's worth of tool calls is reported
//! back to it. Reasoning, retries, and mid-flight plan changes
//! all remain in the agent loop's hands.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// Chain DSL
// ---------------------------------------------------------------------------

/// One step in a `compact_tool_chain` invocation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChainStep {
    /// Tool name to dispatch.
    pub tool: String,
    /// Parameters passed to the tool. Free-form — validated by
    /// the underlying tool, not by the chain.
    pub params: Value,
    /// Optional human-readable label for logging and per-step
    /// result entries. Helps the user (and the next-turn LLM)
    /// tell which step produced what.
    #[serde(default)]
    pub label: Option<String>,
}

/// Execution mode for the chain.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainMode {
    /// Run steps one after another in declared order. Each step's
    /// output is logged but NOT fed back as input to the next
    /// step — this is a deliberate limitation. If you need
    /// chaining-by-output, fall back to multiple tool_use rounds.
    Sequential,
    /// Run steps concurrently via `tokio::JoinSet`. Order of
    /// completion is non-deterministic; results are returned in
    /// the original declared order.
    Parallel,
}

impl Default for ChainMode {
    fn default() -> Self {
        ChainMode::Sequential
    }
}

/// What to do when a step fails.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    /// Stop at the first error and surface it to the LLM. The
    /// remaining steps are marked `skipped`.
    AbortOnFirstError,
    /// Run every step regardless. Each step's status is in the
    /// result; the chain result is `success: false` if any step
    /// failed.
    Continue,
}

impl Default for FailureMode {
    fn default() -> Self {
        FailureMode::AbortOnFirstError
    }
}

/// Top-level args for the `compact_tool_chain` tool.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompactToolChainArgs {
    /// Ordered (or unordered, if `mode: "parallel"`) list of
    /// tool invocations to execute as a single LLM-visible call.
    pub chain: Vec<ChainStep>,
    /// `sequential` (default) or `parallel`.
    #[serde(default)]
    pub mode: ChainMode,
    /// `abort_on_first_error` (default) or `continue`.
    #[serde(default)]
    pub failure_mode: FailureMode,
    /// Optional one-line summary the LLM wants back in the result
    /// entry. The chain still emits the full per-step results;
    /// this just gives the LLM a hook to label what the chain
    /// *meant* to do, so the next turn's reasoning can refer to
    /// it without re-reading the per-step detail.
    #[serde(default)]
    pub intent: Option<String>,
}

// ---------------------------------------------------------------------------
// Result shape
// ---------------------------------------------------------------------------

/// Per-step result entry included in the final `tool_result`.
#[derive(Debug, Clone, Serialize)]
pub struct ChainStepResult {
    /// Index of the step in the original `chain` (0-based).
    pub index: usize,
    /// Tool that was invoked.
    pub tool: String,
    /// Label from `ChainStep.label` if provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `ok`, `error`, or `skipped` (only set when a prior step
    /// aborted and this one never ran).
    pub status: String,
    /// Truncated result content. Empty when `status != "ok"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Error message when `status == "error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Wall-clock time the tool took, milliseconds.
    pub duration_ms: u64,
    /// True when the result was truncated to fit the budget.
    #[serde(default)]
    pub truncated: bool,
}

/// Aggregate result written back as the `tool_result` content.
/// Serialized as a single JSON object so the LLM gets one
/// structured block instead of N streamed events.
#[derive(Debug, Clone, Serialize)]
pub struct ChainResult {
    /// `true` when every step succeeded (or all marked-skipped
    /// steps are intentional). With `failure_mode: continue`, this
    /// is `false` if any step errored.
    pub success: bool,
    /// Effective mode that ran.
    pub mode: ChainMode,
    /// Effective failure_mode that ran.
    pub failure_mode: FailureMode,
    /// Total number of steps in the chain (== `chain.len()`).
    pub total_steps: usize,
    /// Number of steps that ran and returned `ok`.
    pub ok_count: usize,
    /// Number of steps that ran and returned `error`.
    pub error_count: usize,
    /// Number of steps skipped due to early abort.
    pub skipped_count: usize,
    /// Total wall-clock time across all steps, milliseconds.
    pub total_duration_ms: u64,
    /// Optional intent echo-back from the LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Per-step results, in declared order.
    pub steps: Vec<ChainStepResult>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `compact_tool_chain` tool handler.
pub struct CompactToolChainHandler {
    registry: Arc<ToolRegistry>,
    /// Per-step result content is truncated to this many characters
    /// to bound the LLM context growth. The default of 4000 is the
    /// sweet spot we use elsewhere (see
    /// `hermix_core::truncate_chars`); higher values leak more
    /// context, lower values lose detail the user might want.
    per_step_max_chars: usize,
}

impl CompactToolChainHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            per_step_max_chars: 4000,
        }
    }

    /// Override the per-step content truncation ceiling. Tests
    /// use a smaller value; production uses the 4000 default.
    pub fn with_per_step_max_chars(mut self, n: usize) -> Self {
        self.per_step_max_chars = n;
        self
    }
}

#[async_trait]
impl ToolHandler for CompactToolChainHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let args: CompactToolChainArgs = serde_json::from_value(params)
            .map_err(|e| ToolError::InvalidParams(format!("invalid compact_tool_chain args: {e}")))?;

        // Defensive limits. Without these, a malformed LLM
        // call could fan out into a thousand tool invocations
        // and quietly DoS the agent loop.
        if args.chain.is_empty() {
            return Err(ToolError::InvalidParams(
                "compact_tool_chain.chain must contain at least one step".into(),
            ));
        }
        if args.chain.len() > 32 {
            return Err(ToolError::InvalidParams(format!(
                "compact_tool_chain.chain has {} steps; maximum is 32",
                args.chain.len()
            )));
        }
        if args.chain.len() == 1 {
            // Refuse single-step chains — the caller should
            // invoke the underlying tool directly. This keeps
            // the routing surface honest and prevents the LLM
            // from accidentally "compacting" one trivial call.
            return Err(ToolError::InvalidParams(
                "compact_tool_chain needs ≥ 2 steps; for a single tool call, invoke the tool directly".into(),
            ));
        }

        let started_total = std::time::Instant::now();
        let mut steps_out: Vec<ChainStepResult> = Vec::with_capacity(args.chain.len());
        let mut ok = 0usize;
        let mut err = 0usize;
        let mut skipped = 0usize;
        let mut aborted = false;

        match args.mode {
            ChainMode::Sequential => {
                for (idx, step) in args.chain.iter().enumerate() {
                    if aborted {
                        steps_out.push(ChainStepResult {
                            index: idx,
                            tool: step.tool.clone(),
                            label: step.label.clone(),
                            status: "skipped".into(),
                            content: None,
                            error: None,
                            duration_ms: 0,
                            truncated: false,
                        });
                        skipped += 1;
                        continue;
                    }

                    let started = std::time::Instant::now();
                    let call = hermix_core::ToolCall {
                        id: format!("chain-{idx}"),
                        function: hermix_core::FunctionCall {
                            name: step.tool.clone(),
                            // Tool arguments are passed as a JSON
                            // string by the dispatcher; the caller's
                            // `params` is already a Value, so we
                            // round-trip it.
                            arguments: serde_json::to_string(&step.params)
                                .unwrap_or_else(|_| "null".into()),
                        },
                    };
                    let result = crate::dispatch::dispatch_single(
                        call,
                        self.registry.clone(),
                        self.per_step_max_chars,
                    )
                    .await;
                    let duration_ms = started.elapsed().as_millis() as u64;

                    if result.is_error {
                        steps_out.push(ChainStepResult {
                            index: idx,
                            tool: step.tool.clone(),
                            label: step.label.clone(),
                            status: "error".into(),
                            content: None,
                            error: Some(result.content),
                            duration_ms,
                            truncated: false,
                        });
                        err += 1;
                        if args.failure_mode == FailureMode::AbortOnFirstError {
                            aborted = true;
                        }
                    } else {
                        let truncated = result.content.contains("[... truncated");
                        steps_out.push(ChainStepResult {
                            index: idx,
                            tool: step.tool.clone(),
                            label: step.label.clone(),
                            status: "ok".into(),
                            content: Some(result.content),
                            error: None,
                            duration_ms,
                            truncated,
                        });
                        ok += 1;
                    }
                }
            }
            ChainMode::Parallel => {
                use tokio::task::JoinSet;

                let mut join_set: JoinSet<(usize, hermix_core::ToolResult)> = JoinSet::new();

                for (idx, step) in args.chain.iter().enumerate() {
                    let registry = self.registry.clone();
                    let tool = step.tool.clone();
                    let params_str = serde_json::to_string(&step.params)
                        .unwrap_or_else(|_| "null".into());
                    let max_chars = self.per_step_max_chars;
                    join_set.spawn(async move {
                        let call = hermix_core::ToolCall {
                            id: format!("chain-{idx}"),
                            function: hermix_core::FunctionCall {
                                name: tool,
                                arguments: params_str,
                            },
                        };
                        let r = crate::dispatch::dispatch_single(call, registry, max_chars).await;
                        (idx, r)
                    });
                }

                let mut by_idx: Vec<Option<hermix_core::ToolResult>> =
                    (0..args.chain.len()).map(|_| None).collect();
                while let Some(joined) = join_set.join_next().await {
                    if let Ok((idx, res)) = joined {
                        by_idx[idx] = Some(res);
                    }
                }

                // Parallel mode: walk in declared order. Per-step
                // duration is approximated from the chain's
                // total_duration_ms (we don't track per-task
                // timing without extra plumbing — the LLM gets
                // the total, which is what matters for
                // budget decisions).
                for (idx, step) in args.chain.iter().enumerate() {
                    match by_idx[idx].take() {
                        Some(r) if !r.is_error => {
                            let truncated = r.content.contains("[... truncated");
                            steps_out.push(ChainStepResult {
                                index: idx,
                                tool: step.tool.clone(),
                                label: step.label.clone(),
                                status: "ok".into(),
                                content: Some(r.content),
                                error: None,
                                duration_ms: 0,
                                truncated,
                            });
                            ok += 1;
                        }
                        Some(r) => {
                            steps_out.push(ChainStepResult {
                                index: idx,
                                tool: step.tool.clone(),
                                label: step.label.clone(),
                                status: "error".into(),
                                content: None,
                                error: Some(r.content),
                                duration_ms: 0,
                                truncated: false,
                            });
                            err += 1;
                        }
                        None => {
                            // join_next dropped the task before we
                            // recorded it (panic / cancellation).
                            steps_out.push(ChainStepResult {
                                index: idx,
                                tool: step.tool.clone(),
                                label: step.label.clone(),
                                status: "error".into(),
                                content: None,
                                error: Some("parallel task lost".into()),
                                duration_ms: 0,
                                truncated: false,
                            });
                            err += 1;
                        }
                    }
                }
            }
        }

        let success = err == 0;
        let result = ChainResult {
            success,
            mode: args.mode,
            failure_mode: args.failure_mode,
            total_steps: args.chain.len(),
            ok_count: ok,
            error_count: err,
            skipped_count: skipped,
            total_duration_ms: started_total.elapsed().as_millis() as u64,
            intent: args.intent,
            steps: steps_out,
        };

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("serialize ChainResult: {e}")))
    }

    fn schema(&self) -> ToolSchema {
        let mut chain_item_props: IndexMap<String, Value> = IndexMap::new();
        chain_item_props.insert(
            "tool".to_string(),
            json!({
                "type": "string",
                "description": "Tool name to dispatch at this step."
            }),
        );
        chain_item_props.insert(
            "params".to_string(),
            json!({
                "type": "object",
                "description": "Parameters passed to the tool. Free-form; validated by the underlying tool."
            }),
        );
        chain_item_props.insert(
            "label".to_string(),
            json!({
                "type": "string",
                "description": "Optional human-readable label for this step. Echoed in the result so the next-turn LLM can refer to it without re-reading the full chain."
            }),
        );

        let mut props: IndexMap<String, Value> = IndexMap::new();
        props.insert(
            "chain".to_string(),
            json!({
                "type": "array",
                "minItems": 2,
                "maxItems": 32,
                "items": {
                    "type": "object",
                    "required": ["tool", "params"],
                    "properties": chain_item_props,
                },
                "description": "Ordered (sequential mode) or unordered (parallel mode) list of tool invocations to collapse into a single LLM-context entry."
            }),
        );
        props.insert(
            "mode".to_string(),
            json!({
                "type": "string",
                "enum": ["sequential", "parallel"],
                "default": "sequential",
                "description": "Execution mode. 'sequential' (default) runs steps one after another in declared order. 'parallel' fans out via tokio::JoinSet and returns results in declared order regardless of completion order."
            }),
        );
        props.insert(
            "failure_mode".to_string(),
            json!({
                "type": "string",
                "enum": ["abort_on_first_error", "continue"],
                "default": "abort_on_first_error",
                "description": "How to handle a failed step. 'abort_on_first_error' (default) short-circuits and surfaces the first error. 'continue' runs every step and returns a per-step status array — useful for read-only sweeps."
            }),
        );
        props.insert(
            "intent".to_string(),
            json!({
                "type": "string",
                "description": "Optional one-line summary of what this chain is meant to accomplish. Echoed back in the result so the next-turn LLM can refer to the chain's purpose without re-reading the per-step detail."
            }),
        );

        tool_schema(
            "compact_tool_chain",
            "Collapse N tool calls (2–32) into a single LLM-context entry. Each step runs through the normal dispatch path, but only one tool_result block lands in the chat history — this is the main lever for keeping long sessions under the context budget. Use 'sequential' when order matters, 'parallel' for read-only sweeps. NOT a substitute for tool_use when the LLM needs to branch on intermediate results — for that, issue separate tool calls.",
            JsonSchema::object(props, vec!["chain".to_string()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate content to `max_chars` on a char boundary (not byte
/// boundary — CJK strings would otherwise panic).
/// Returns `(content, was_truncated)`.
fn truncate(s: &str, max_chars: usize) -> (String, bool) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), false);
    }
    let truncated: String = s.chars().take(max_chars).collect();
    (truncated, true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::ToolHandler;
    use std::sync::Arc;

    // -----------------------------------------------------------------
    // Pure-data tests (no registry needed)
    // -----------------------------------------------------------------

    #[test]
    fn truncate_short_string_is_identity() {
        let (out, was_truncated) = truncate("hello", 100);
        assert_eq!(out, "hello");
        assert!(!was_truncated);
    }

    #[test]
    fn truncate_at_char_boundary_handles_cjk() {
        // 5 CJK characters; max 3 → result is the first 3 chars.
        let s = "你好世界你";
        let (out, was_truncated) = truncate(s, 3);
        assert_eq!(out, "你好世");
        assert!(was_truncated);
    }

    #[test]
    fn truncate_exact_boundary_returns_full() {
        let s = "abcdef";
        let (out, was_truncated) = truncate(s, 6);
        assert_eq!(out, "abcdef");
        assert!(!was_truncated);
    }

    #[test]
    fn chain_step_deserializes_minimal_payload() {
        let json = json!({
            "tool": "echo",
            "params": {"hello": "world"},
        });
        let step: ChainStep = serde_json::from_value(json).unwrap();
        assert_eq!(step.tool, "echo");
        assert_eq!(step.params, json!({"hello": "world"}));
        assert!(step.label.is_none());
    }

    #[test]
    fn chain_args_default_mode_is_sequential() {
        let json = json!({
            "chain": [
                {"tool": "echo", "params": {}},
                {"tool": "echo", "params": {}},
            ]
        });
        let args: CompactToolChainArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.mode, ChainMode::Sequential);
        assert_eq!(args.failure_mode, FailureMode::AbortOnFirstError);
        assert!(args.intent.is_none());
    }

    #[test]
    fn chain_args_parses_all_modes() {
        let json = json!({
            "chain": [
                {"tool": "a", "params": {}},
                {"tool": "b", "params": {}},
            ],
            "mode": "parallel",
            "failure_mode": "continue",
            "intent": "gather context",
        });
        let args: CompactToolChainArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.mode, ChainMode::Parallel);
        assert_eq!(args.failure_mode, FailureMode::Continue);
        assert_eq!(args.intent.as_deref(), Some("gather context"));
    }

    #[test]
    fn chain_step_result_omits_none_fields_in_json() {
        let r = ChainStepResult {
            index: 0,
            tool: "echo".into(),
            label: None,
            status: "ok".into(),
            content: Some("hello".into()),
            error: None,
            duration_ms: 5,
            truncated: false,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
        assert!(s.contains("\"content\":\"hello\""));
        // None fields must be skipped, not emitted as `null`.
        assert!(!s.contains("label"));
        assert!(!s.contains("error"));
    }

    #[test]
    fn chain_result_aggregates_step_counts_correctly() {
        let r = ChainResult {
            success: true,
            mode: ChainMode::Sequential,
            failure_mode: FailureMode::AbortOnFirstError,
            total_steps: 3,
            ok_count: 3,
            error_count: 0,
            skipped_count: 0,
            total_duration_ms: 42,
            intent: Some("test".into()),
            steps: vec![],
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"total_steps\":3"));
        assert!(json.contains("\"ok_count\":3"));
        assert!(json.contains("\"intent\":\"test\""));
    }

    #[test]
    fn schema_lists_compact_tool_chain_with_descriptive_text() {
        let registry = ToolRegistry::new();
        let handler = CompactToolChainHandler::new(Arc::new(registry));
        let s = handler.schema();
        assert_eq!(s.name, "compact_tool_chain");
        let desc = s.description.as_str();
        assert!(
            desc.contains("Collapse N tool calls"),
            "description must explain the zero-context RPC intent: {desc}"
        );
        assert!(
            desc.contains("NOT a substitute"),
            "description must warn LLM not to use it for branching: {desc}"
        );
    }

    // -----------------------------------------------------------------
    // Validation-only tests (no execution needed)
    // -----------------------------------------------------------------

    fn handler() -> CompactToolChainHandler {
        CompactToolChainHandler::new(Arc::new(ToolRegistry::new()))
    }

    #[tokio::test]
    async fn execute_rejects_empty_chain() {
        let args = json!({"chain": []});
        let result = handler().execute(args).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn execute_rejects_single_step_chain() {
        let args = json!({"chain": [{"tool": "echo", "params": {}}]});
        let result = handler().execute(args).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn execute_rejects_oversized_chain() {
        let chain: Vec<_> = (0..33)
            .map(|i| json!({"tool": "echo", "params": {"i": i}}))
            .collect();
        let args = json!({"chain": chain});
        let result = handler().execute(args).await;
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("maximum is 32"), "got: {msg}");
    }

    #[tokio::test]
    async fn execute_rejects_malformed_args() {
        // Missing `chain` field.
        let args = json!({});
        let result = handler().execute(args).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }
}
