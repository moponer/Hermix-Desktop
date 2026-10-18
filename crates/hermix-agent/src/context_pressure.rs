//! Context-pressure warning state.
//!
//! : the 3 sibling fields (`warned_tier`,
//! `last_warn_at`, `last_warn_percent`) that used to live inline on
//! `LoopState` are folded into [`ContextPressureMonitor`]. The shared
//! [`ContextPressureMonitor::should_emit_warning`] method owns the
//! cooldown + tier-upgrade + percent-step rules and is unit-testable
//! without spinning up an `AgentLoop`.
//!
//! Physically split out of `agent_loop.rs` so the file
//! that owns the type also owns the type — no API change to callers
//! (`state.context_pressure.*` call paths work unchanged).
//!
//! Fields stay `pub(crate)` so `agent_loop.rs` test code (which pokes
//! them directly to assert invariants) keeps compiling without a
//! helper-method dance.

use std::time::{Duration, Instant};

/// Encapsulates the 3 tracking fields the context-pressure warning
/// logic uses. Lifted out of `LoopState` so the cooldown/throttle
/// rules live in one place (and can be unit-tested without spinning
/// up an `AgentLoop`).
///
/// replaces 3 fields (`context_pressure_warned_at`,
/// `context_pressure_last_warn_at`, `context_pressure_last_warn_percent`)
/// scattered across `LoopState`. The shared [`should_emit_warning`]
/// method now owns the cooldown + tier-upgrade + percent-step rules.
#[derive(Debug, Clone, Default)]
pub struct ContextPressureMonitor {
    /// Tier at which we last warned (0.0 = never). Different tiers
    /// represent different pressure thresholds crossed (e.g. 0.5, 0.7, 0.85).
    pub(crate) warned_tier: f64,
    /// Wall-clock instant of the last warning (None = never).
    pub(crate) last_warn_at: Option<Instant>,
    /// Last progress percentage at which we emitted a warning.
    pub(crate) last_warn_percent: f64,
    /// Turn numbers of recent successful compactions (rapid-refill
    /// detection, ZCode evaluateRapidRefill analog).
    pub(crate) compact_turns: Vec<u32>,
}

impl ContextPressureMonitor {
    /// Record a successful compaction at `turn`. Returns `true` when the
    /// refill pace is pathological — **≥3 compactions within the last 5
    /// turns** — meaning something is re-filling the context faster than
    /// compression can shrink it (e.g. a tool dumping a huge blob every
    /// turn). Callers hard-stop the run instead of burning tokens on a
    /// compression storm.
    pub(crate) fn record_compaction(&mut self, turn: u32) -> bool {
        self.compact_turns.push(turn);
        self.compact_turns.retain(|t| turn.saturating_sub(*t) <= 4);
        self.compact_turns.len() >= 3
    }
}

impl ContextPressureMonitor {
    /// Returns true if we should emit a context-pressure warning for the
    /// current `progress_ratio` (0.0..=1.0) at the given `tier`.
    /// Rules (unchanged from the pre-extraction free function):
    /// - `tier <= 0.0` is a no-op (caller suppressed this tier).
    /// - Emit when the tier increased OR (cooldown elapsed AND
    /// progress advanced by ≥ `WARN_PERCENT_STEP` since last warn).
    /// - On emit: store `tier` (only when it changed), `now`,
    /// and `progress_percent`.
    pub fn should_emit_warning(&mut self, progress_ratio: f64, tier: f64) -> bool {
        if tier <= 0.0 {
            return false;
        }
        let progress_percent = progress_ratio * 100.0;
        let now = Instant::now();
        const WARN_COOLDOWN_SECS: u64 = 20;
        const WARN_PERCENT_STEP: f64 = 5.0;

        let tier_upgraded = tier > self.warned_tier;
        let cooldown_elapsed = self
            .last_warn_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(WARN_COOLDOWN_SECS))
            .unwrap_or(true);
        let percent_advanced =
            (progress_percent - self.last_warn_percent) >= WARN_PERCENT_STEP;

        if tier_upgraded || (cooldown_elapsed && percent_advanced) {
            if tier_upgraded {
                self.warned_tier = tier;
            }
            self.last_warn_at = Some(now);
            self.last_warn_percent = progress_percent;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_refill_trips_on_three_compactions_in_five_turns() {
        let mut m = ContextPressureMonitor::default();
        assert!(!m.record_compaction(10));
        assert!(!m.record_compaction(12));
        // 第三次（5 轮窗口内）→ 熔断。
        assert!(m.record_compaction(14));
        // 老记录滑出窗口后自动恢复。
        assert!(!m.record_compaction(19));
    }


    #[test]
    fn default_state_emits_no_warning_until_conditions_met() {
        let mut m = ContextPressureMonitor::default();
        // tier <= 0.0 short-circuits
        assert!(!m.should_emit_warning(0.5, 0.0));
        assert!(!m.should_emit_warning(0.5, -0.1));
        // first emit must beat both tier-upgrade AND cooldown+
        assert!(m.should_emit_warning(0.5, 0.5));
        assert_eq!(m.warned_tier, 0.5);
        assert_eq!(m.last_warn_percent, 50.0);
    }

    #[test]
    fn tier_upgrade_emits_even_before_cooldown_elapses() {
        let mut m = ContextPressureMonitor::default();
        assert!(m.should_emit_warning(0.5, 0.5));
        // Different (higher) tier bypasses cooldown entirely.
        assert!(m.should_emit_warning(0.55, 0.85));
        assert_eq!(m.warned_tier, 0.85);
    }

    #[test]
    fn same_tier_within_cooldown_does_not_re_emit() {
        let mut m = ContextPressureMonitor::default();
        assert!(m.should_emit_warning(0.5, 0.5));
        // Same tier, no cooldown elapsed, percent advanced < 5.0.
        assert!(!m.should_emit_warning(0.51, 0.5));
        // same tier, percent advanced ≥ 5.0 but no cooldown elapsed.
        assert!(!m.should_emit_warning(0.56, 0.5));
    }

    #[test]
    fn last_warn_at_records_after_emit() {
        let mut m = ContextPressureMonitor::default();
        assert!(m.last_warn_at.is_none());
        assert!(m.should_emit_warning(0.5, 0.5));
        assert!(m.last_warn_at.is_some());
    }
}
