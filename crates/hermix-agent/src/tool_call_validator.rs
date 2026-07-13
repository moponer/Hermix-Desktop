//! Unified tool-call validation (AL-7, 2026-07-09).
//!
//! Previously the tool-call validation logic (invalid name check +
//! invalid JSON check) was copy-pasted between `run()` and
//! `run_stream()` in `run_loop.rs` — ~110 lines of nearly identical
//! code in each method. This module centralizes that logic into a
//! single `validate` function that returns a `ValidationOutcome`,
//! letting the caller handle loop control flow (`continue` / `return`)
//! without duplicating the validation checks.
//!
//! ## Validation pipeline
//! 1. **Invalid tool name** — check each `tc.function.name` against
//!    `tool_registry`. If any are unknown, bump the invalid-tool retry
//!    counter. If max retries exceeded → `FatalInvalidTool`. Otherwise
//!    → `RetryInvalidTool` (caller pushes error results and `continue`s).
//! 2. **Invalid JSON arguments** — normalize each tool call's arguments.
//!    If any fail to parse, bump the invalid-JSON retry counter. If max
//!    retries exceeded → return error results. Otherwise →
//!    `RetryInvalidJson` (caller pops last message and `continue`s).
//! 3. **All valid** → `Valid` (caller proceeds to dedup + execution).

use hermix_core::{Message, ToolCall};

use crate::context::ContextManager;
use crate::retry_counters::RetryCounters;
use crate::tool_result_processor::push_tool_result;

/// Outcome of validating a batch of tool calls.
#[derive(Debug)]
pub(crate) enum ValidationOutcome {
    /// All tool calls passed validation. Caller proceeds to dedup +
    /// execution.
    Valid,
    /// One or more tool calls have unknown names. Caller should push
    /// error `tool_result` messages for each call and `continue` the
    /// loop. The `error_contents` vec aligns 1:1 with `tool_calls`.
    RetryInvalidTool {
        error_contents: Vec<String>,
    },
    /// Max invalid-tool retries reached. Caller should add the system
    /// message and return the final `AgentResult`.
    FatalInvalidTool {
        invalid_names: Vec<String>,
    },
    /// One or more tool calls have unparseable JSON arguments. Caller
    /// should pop the last assistant message, recompute tokens, and
    /// `continue` the loop.
    RetryInvalidJson,
    /// Max invalid-JSON retries reached. Caller should push error
    /// `tool_result` messages for each call and `continue` the loop.
    /// The `error_contents` vec aligns 1:1 with `tool_calls`.
    FatalInvalidJson {
        error_contents: Vec<String>,
    },
}

/// Configuration needed for validation.
pub(crate) struct ValidatorConfig<'a> {
    pub max_invalid_tool_retries: u32,
    pub max_invalid_json_retries: u32,
    pub available_tools: &'a str,
}

/// Validate a batch of tool calls. Mutates `tool_calls` in place
/// (normalizes arguments). Mutates `retry_counters` (bumps/resets as needed).
///
/// **Pre-condition**: callers must repair + hydrate tool calls BEFORE
/// calling this function (the mute/housekeeping check in `run_stream`
/// needs to see repaired names).
///
/// Returns the outcome — the caller is responsible for side effects
/// (pushing error results, popping messages, loop control).
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate(
    tool_calls: &mut Vec<ToolCall>,
    tool_registry_get: impl Fn(&str) -> bool,
    normalize_fn: impl Fn(&mut ToolCall) -> Result<(), String>,
    retry_counters: &mut RetryCounters,
    config: &ValidatorConfig,
) -> ValidationOutcome {
    // --- Step 1: Invalid tool name check ---
    let invalid_tool_calls: Vec<String> = tool_calls
        .iter()
        .filter(|tc| !tool_registry_get(&tc.function.name))
        .map(|tc| tc.function.name.clone())
        .collect();

    if !invalid_tool_calls.is_empty() {
        let count = retry_counters.bump_invalid_tool();
        if count >= config.max_invalid_tool_retries {
            return ValidationOutcome::FatalInvalidTool {
                invalid_names: invalid_tool_calls,
            };
        }
        // Build error contents for each tool call
        let error_contents: Vec<String> = tool_calls
            .iter()
            .map(|tc| {
                if !tool_registry_get(&tc.function.name) {
                    format!(
                        "Tool '{}' does not exist. Available tools: {}",
                        tc.function.name, config.available_tools
                    )
                } else {
                    "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                }
            })
            .collect();
        return ValidationOutcome::RetryInvalidTool { error_contents };
    }
    retry_counters.reset_invalid_tool();

    // --- Step 2: Invalid JSON arguments check ---
    let mut invalid_json_args: Vec<(String, String)> = Vec::new();
    for tc in tool_calls.iter_mut() {
        if let Err(e) = normalize_fn(tc) {
            invalid_json_args.push((tc.function.name.clone(), e));
        }
    }

    if !invalid_json_args.is_empty() {
        let count = retry_counters.bump_invalid_json();
        if count < config.max_invalid_json_retries {
            return ValidationOutcome::RetryInvalidJson;
        }
        // Max retries — build error contents
        retry_counters.reset_invalid_json();
        let error_contents: Vec<String> = tool_calls
            .iter()
            .map(|tc| {
                if let Some((_, err)) = invalid_json_args
                    .iter()
                    .find(|(name, _)| name == &tc.function.name)
                {
                    format!(
                        "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                        err
                    )
                } else {
                    "Skipped: other tool call in this response had invalid JSON.".to_string()
                }
            })
            .collect();
        return ValidationOutcome::FatalInvalidJson { error_contents };
    }
    retry_counters.reset_invalid_json();

    ValidationOutcome::Valid
}

/// Result of applying a `ValidationOutcome` to the context.
/// Tells the caller exactly what loop action to take.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApplyResult {
    /// Validation passed — proceed to tool execution.
    Proceed,
    /// Error results pushed / message popped — `continue` the loop.
    Continue,
    /// Fatal error (max retries) — system message added; caller must
    /// `return` the final `AgentResult`.
    ReturnFatal,
}

/// Apply a `ValidationOutcome` to the context, performing the side
/// effects (pushing error results, adding system messages).
///
/// Returns an [`ApplyResult`] telling the caller exactly what to do.
pub(crate) fn apply_outcome(
    ctx: &mut ContextManager,
    tool_calls: &[ToolCall],
    outcome: ValidationOutcome,
) -> ApplyResult {
    match outcome {
        ValidationOutcome::Valid => ApplyResult::Proceed,
        ValidationOutcome::RetryInvalidTool { error_contents } => {
            for (tc, content) in tool_calls.iter().zip(error_contents.iter()) {
                push_tool_result(ctx, tool_calls, &tc.id, content);
            }
            ApplyResult::Continue
        }
        ValidationOutcome::FatalInvalidTool { invalid_names } => {
            ctx.add_message(Message::system(format!(
                "Max invalid tool retries reached. Last invalid tool: {}",
                invalid_names.first().unwrap_or(&"unknown".to_string())
            )));
            ApplyResult::ReturnFatal
        }
        ValidationOutcome::RetryInvalidJson => {
            let _ = ctx.get_messages_mut().pop();
            ctx.recompute_total_tokens();
            ApplyResult::Continue
        }
        ValidationOutcome::FatalInvalidJson { error_contents } => {
            for (tc, content) in tool_calls.iter().zip(error_contents.iter()) {
                push_tool_result(ctx, tool_calls, &tc.id, content);
            }
            ApplyResult::Continue // pushes errors, lets LLM see them
        }
    }
}
