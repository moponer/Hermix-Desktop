//! Per-loop retry counters for malformed / truncated tool-call recovery.
//!
//! Phase 5 (2026-07-03): 3 sibling `u32` fields (`invalid_tool`,
//! `invalid_json`, `truncated_tool`) consolidated into one struct so
//! `LoopState` doesn't carry them as siblings and `publish_from`
//! doesn't take 3 sibling `u32` arguments.
//!
//! Physically split out of `agent_loop.rs` (2026-07-04); API unchanged.
//! Fields stay `pub(crate)` so existing `state.retry_counters.*` call
//! paths (run-stream truncation handling, invalid-tool recovery, and
//! the `LoopState::default` invariant test) keep compiling without a
//! helper-method dance.

/// Per-loop retry counters for malformed / truncated tool-call recovery.
///
/// All three counters follow the same shape:
/// - `bump_*` saturating-increments and returns the new value.
/// - `reset_*` clears the counter (called on the success path).
/// - The `< max` / `>= max` comparison stays at the call site because
/// the action on max differs between the three (finalize vs.
/// continue-with-error vs. fall-through) — folding that decision
/// into the helper would force one path on all three.
#[derive(Debug, Clone, Default)]
pub struct RetryCounters {
    /// Retries against invalid tool names.
    pub(crate) invalid_tool: u32,
    /// Retries against invalid JSON in tool arguments.
    pub(crate) invalid_json: u32,
    /// Retries when the streaming provider truncated a tool call's JSON.
    /// `run_stream()`-only — non-stream `run()` cannot observe truncation.
    pub(crate) truncated_tool: u32,
}

impl RetryCounters {
    /// Saturating-increment the invalid tool name counter and return the
    /// new value. Caller compares to `config.invalid_tool_call_max_retries`.
    pub(crate) fn bump_invalid_tool(&mut self) -> u32 {
        self.invalid_tool = self.invalid_tool.saturating_add(1);
        self.invalid_tool
    }

    /// Saturating-increment the invalid JSON counter and return the new
    /// value. Caller compares to `config.invalid_tool_json_max_retries`.
    pub(crate) fn bump_invalid_json(&mut self) -> u32 {
        self.invalid_json = self.invalid_json.saturating_add(1);
        self.invalid_json
    }

    /// Saturating-increment the truncated tool call counter and return the
    /// new value. Caller compares to
    /// `config.truncated_tool_call_max_retries`.
    pub(crate) fn bump_truncated_tool(&mut self) -> u32 {
        self.truncated_tool = self.truncated_tool.saturating_add(1);
        self.truncated_tool
    }

    pub(crate) fn reset_invalid_tool(&mut self) {
        self.invalid_tool = 0;
    }

    pub(crate) fn reset_invalid_json(&mut self) {
        self.invalid_json = 0;
    }

    pub(crate) fn reset_truncated_tool(&mut self) {
        self.truncated_tool = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero() {
        let c = RetryCounters::default();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    fn bumps_saturate_and_return_new_value() {
        let mut c = RetryCounters::default();
        assert_eq!(c.bump_invalid_tool(), 1);
        assert_eq!(c.bump_invalid_tool(), 2);
        assert_eq!(c.bump_invalid_json(), 1);
        assert_eq!(c.bump_truncated_tool(), 1);
        // state preserved across bumps
        assert_eq!(c.invalid_tool, 2);
        assert_eq!(c.invalid_json, 1);
        assert_eq!(c.truncated_tool, 1);
    }

    #[test]
    fn resets_clear_their_own_counter_only() {
        let mut c = RetryCounters::default();
        c.bump_invalid_tool();
        c.bump_invalid_json();
        c.bump_truncated_tool();
        c.reset_invalid_tool();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 1);
        assert_eq!(c.truncated_tool, 1);
    }

    #[test]
    fn saturating_add_protects_against_u32_overflow() {
        let mut c = RetryCounters {
            invalid_tool: u32::MAX,
            ..RetryCounters::default()
        };
        // bumps at max stay at max — no overflow panic
        assert_eq!(c.bump_invalid_tool(), u32::MAX);
        assert_eq!(c.bump_invalid_tool(), u32::MAX);
    }
}
