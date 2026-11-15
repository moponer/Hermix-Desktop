//! Per-run turn-level timing accumulators.
//!
//! (follow-up): the 2 sibling `u64` fields
//! (`total_api_time_ms`, `total_tool_time_ms`) that used to live
//! directly on `LoopState` were bridged back from `run()`'s
//! loop-locals via 2 sibling arguments in `publish_from`. Folding
//! them into one struct shrinks `publish_from` by 2 args and puts
//! the timing mutation next to the data (`run()` calls
//! `record_api` / `record_tool` directly).
//!
//! Physically split out of `agent_loop.rs` ; API contract
//! unchanged. Fields stay `pub(crate)` so callers outside the
//! original inline impl block (currently none) can read them without
//! churning accessor surface.

/// Per-run turn-level timing accumulators.
///
/// The 2 fields stay co-located on `self.timings` and mutate in
/// place. Reset / `Default` produce the same all-zeros state as
/// before, so `LoopState::default()` semantics are unchanged.
#[derive(Debug, Clone, Default)]
pub struct TurnTimings {
    /// Accumulated LLM-call wall-clock time in milliseconds across
    /// all turns of this run.
    pub(crate) total_api_time_ms: u64,
    /// Accumulated tool-execution wall-clock time in milliseconds
    /// across all turns of this run.
    pub(crate) total_tool_time_ms: u64,
}

impl TurnTimings {
    /// Accumulate a per-LLM-call elapsed time into the running API
    /// total. Saturating-add so a long-running session can't wrap
    /// around.
    pub(crate) fn record_api(&mut self, elapsed_ms: u64) {
        self.total_api_time_ms = self.total_api_time_ms.saturating_add(elapsed_ms);
    }

    /// Accumulate a per-tool-execution elapsed time into the running
    /// tool total. Saturating-add.
    pub(crate) fn record_tool(&mut self, elapsed_ms: u64) {
        self.total_tool_time_ms = self.total_tool_time_ms.saturating_add(elapsed_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero() {
        let t = TurnTimings::default();
        assert_eq!(t.total_api_time_ms, 0);
        assert_eq!(t.total_tool_time_ms, 0);
    }

    #[test]
    fn record_api_saturating_adds() {
        let mut t = TurnTimings::default();
        t.record_api(100);
        t.record_api(50);
        assert_eq!(t.total_api_time_ms, 150);
        assert_eq!(t.total_tool_time_ms, 0);
    }

    #[test]
    fn record_tool_saturating_adds() {
        let mut t = TurnTimings::default();
        t.record_tool(40);
        t.record_tool(60);
        assert_eq!(t.total_tool_time_ms, 100);
        assert_eq!(t.total_api_time_ms, 0);
    }

    #[test]
    fn saturating_add_protects_against_u64_overflow() {
        let mut t = TurnTimings {
            total_api_time_ms: u64::MAX,
            total_tool_time_ms: 0,
        };
        t.record_api(100);
        // stays at max — no panic
        assert_eq!(t.total_api_time_ms, u64::MAX);
    }

    #[test]
    fn records_routes_to_correct_field() {
        let mut t = TurnTimings::default();
        t.record_api(10);
        t.record_tool(20);
        assert_ne!(t.total_api_time_ms, t.total_tool_time_ms);
        assert_eq!(t.total_api_time_ms, 10);
        assert_eq!(t.total_tool_time_ms, 20);
    }
}
