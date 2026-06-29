//! Safety policy layer for `hermix-cua`.
//!
//! Three orthogonal gates:
//!
//! 1. **Blacklist** — key chords that must never be replayed by an
//!    agent (Ctrl+Alt+Del, Win+L, Alt+F4, etc.). Replay of any
//!    blacklisted chord returns [`CuaError::Denied`] before the OS
//!    sees the input.
//! 2. **Rate limiter** — sliding-window cap on actions per second
//!    to protect the OS input thread from runaway loops.
//! 3. **Approval mode** — placeholder flag surfaced to the
//!    `hermix-tools` layer, where the per-tool dispatch gate is the
//!    actual source of truth (`PermissionLevel::Default` already
//!    prompts the user). The flag here exists so the safety guard
//!    can refuse to execute an obviously-destructive action even
//!    before the dispatch gate fires.
//!
//! The guard is `Send + Sync` and intentionally cheap to clone
//! behind an `Arc` (used as a `once_cell::sync::Lazy` global in
//! `hermix-tools/src/tools/cua.rs`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::CuaError;
use crate::CuaResult;

/// How the safety guard should behave at runtime. Mirrors — but
/// does not duplicate — `PermissionLevel` from `hermix-core`. The
/// guard uses this flag to decide whether to refuse an action
/// outright (deny list) versus letting the dispatch gate handle
/// the user prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// One prompt per task; destructive actions prompt again. This
    /// matches `PermissionLevel::Default`.
    TaskLevelOnce,
    /// Every action prompts. Stricter than `Default`; used during
    /// debugging or sensitive sessions.
    EveryAction,
    /// No prompts (the dispatch gate will still prompt at the
    /// `Dangerous` tool boundary unless `PermissionLevel::Full`).
    /// Provided for completeness — the guard never enables this on
    /// its own.
    FullAuto,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        // Mirrors `PermissionLevel::default()` — task-level once
        // is the safe and most-used default.
        ApprovalMode::TaskLevelOnce
    }
}

/// Risk classification used by the safety guard. Distinct from
/// `hermix_core::RiskLevel` (which describes the tool's *general*
/// risk; this one describes a *specific invocation*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Read-only (screenshot, screen_info).
    Low,
    /// Reversible side effect (type text, generic key chord).
    Medium,
    /// Hard-to-reverse (click "confirm", "delete", "shutdown").
    High,
}

/// Key chords the agent is forbidden from replaying. Matched
/// case-insensitively against the public `chord` string the LLM
/// supplied. Strings are normalised by `is_blacklisted` (lowercase,
/// trim, sort the `+`-separated parts).
const BLACKLIST: &[&str] = &[
    // Secure attention sequence — directly handled by the OS,
    // user-mode injection is ignored, but we still refuse to
    // spend the round-trip.
    "ctrl+alt+delete",
    "ctrl+alt+del",
    // Lock workstation — do not let the agent lock the user out
    // of their own machine.
    "win+l",
    "meta+l",
    "super+l",
    // Force-close the foreground window — agents that hit this
    // while iterating tend to kill their own UI.
    "alt+f4",
    // Shutdown / log-off / restart.
    "ctrl+alt+end",
    "win+x",        // opens the power-user menu — closes everything
    "win+d",        // show desktop — destructive to the user's
                    // mental model of which window is active
    "ctrl+win+delete",
];

/// Default rate-limit ceiling: at most 20 actions per second
/// across the whole guard. Tuned to be well above human input
/// rate (~5/sec for fast typers) but low enough that a runaway
/// loop is visibly throttled.
const DEFAULT_RATE_BURST: usize = 20;
const DEFAULT_RATE_WINDOW: Duration = Duration::from_millis(1_000);

/// Sliding-window action-rate limiter. Thread-safe via `Mutex`.
#[derive(Debug)]
struct RateLimiter {
    window: Duration,
    burst: usize,
    /// Timestamps of recent actions (oldest first).
    hits: Vec<Instant>,
}

impl RateLimiter {
    fn new(window: Duration, burst: usize) -> Self {
        Self {
            window,
            burst,
            hits: Vec::with_capacity(burst),
        }
    }

    /// Try to record one hit. Returns `Err` if the cap is exceeded.
    fn try_record(&mut self, now: Instant) -> CuaResult<()> {
        // Drop entries older than the window.
        let cutoff = now
            .checked_sub(self.window)
            .unwrap_or_else(Instant::now);
        while let Some(front) = self.hits.first() {
            if *front < cutoff {
                self.hits.remove(0);
            } else {
                break;
            }
        }
        if self.hits.len() >= self.burst {
            return Err(CuaError::Denied(format!(
                "rate limit exceeded ({} actions in {:?}); slow down",
                self.hits.len(),
                self.window
            )));
        }
        self.hits.push(now);
        Ok(())
    }
}

/// The safety guard. Cheap to construct; clone via `Arc` for
/// sharing across handlers.
#[derive(Debug)]
pub struct SafetyGuard {
    mode: ApprovalMode,
    rate: Mutex<RateLimiter>,
}

impl SafetyGuard {
    /// Construct with the default rate limit and `TaskLevelOnce`
    /// approval mode.
    pub fn new() -> Self {
        Self::with_mode(ApprovalMode::default())
    }

    /// Construct with a specific [`ApprovalMode`] and the default
    /// rate limit.
    pub fn with_mode(mode: ApprovalMode) -> Self {
        Self {
            mode,
            rate: Mutex::new(RateLimiter::new(DEFAULT_RATE_WINDOW, DEFAULT_RATE_BURST)),
        }
    }

    /// Current approval mode.
    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    /// Returns `true` if `chord` (case-insensitive, `+`-separated)
    /// is on the deny list.
    pub fn is_blacklisted(chord: &str) -> bool {
        let normalised = normalise_chord(chord);
        BLACKLIST.iter().any(|bad| normalise_chord(bad) == normalised)
    }

    /// Reject the call if the chord is blacklisted or the rate
    /// limit is exceeded.
    pub fn check(&self, chord: Option<&str>, risk: RiskLevel) -> CuaResult<()> {
        if let Some(c) = chord {
            if Self::is_blacklisted(c) {
                return Err(CuaError::Denied(format!(
                    "key chord '{c}' is on the safety blacklist \
                     (replay would lock or close the desktop)"
                )));
            }
        }
        // Only rate-limit actions that have observable side
        // effects — pure reads (`screenshot`, `screen_info`) are
        // intentionally exempt from the ceiling.
        if matches!(risk, RiskLevel::Medium | RiskLevel::High) {
            let mut rate = self.rate.lock().expect("rate limiter mutex poisoned");
            rate.try_record(Instant::now())?;
        }
        Ok(())
    }

    /// Direct accessor for tests / callers that need to surface
    /// the current rate-limit state.
    #[cfg(test)]
    pub fn hit_count(&self) -> usize {
        self.rate.lock().expect("rate limiter mutex poisoned").hits.len()
    }
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalise a key chord: lowercase, trim, sort the `+`-separated
/// parts. Sorting is what makes `"ctrl+shift+t"` and
/// `"shift+ctrl+t"` compare equal; lowercasing makes the match
/// case-insensitive.
fn normalise_chord(chord: &str) -> String {
    let mut parts: Vec<String> = chord
        .split('+')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    parts.sort_unstable();
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blacklist_matches_case_insensitive() {
        assert!(SafetyGuard::is_blacklisted("Ctrl+Alt+Delete"));
        assert!(SafetyGuard::is_blacklisted("WIN+L"));
        assert!(SafetyGuard::is_blacklisted("alt+f4"));
    }

    #[test]
    fn blacklist_matches_unordered_modifier_order() {
        // The user may supply `alt+ctrl+delete` and the deny list
        // stores `ctrl+alt+delete`. After sort the comparison must
        // match — i.e. modifier *order* does not matter, but the
        // *set* of modifiers does.
        assert!(SafetyGuard::is_blacklisted("alt+ctrl+delete"));
        assert!(SafetyGuard::is_blacklisted("delete+ctrl+alt"));
    }

    #[test]
    fn non_blacklisted_chord_passes() {
        assert!(!SafetyGuard::is_blacklisted("ctrl+c"));
        assert!(!SafetyGuard::is_blacklisted("Return"));
        assert!(!SafetyGuard::is_blacklisted("ctrl+shift+t"));
    }

    #[test]
    fn empty_chord_is_not_blacklisted() {
        // An empty chord string is meaningless but should not
        // false-positive against the deny list (and should not
        // panic the `split`).
        assert!(!SafetyGuard::is_blacklisted(""));
    }

    #[test]
    fn rate_limiter_caps_medium_risk() {
        let g = SafetyGuard::with_mode(ApprovalMode::FullAuto);
        // Fill up the burst with `Medium` risk.
        for _ in 0..DEFAULT_RATE_BURST {
            g.check(None, RiskLevel::Medium).expect("under burst");
        }
        // One more must fail.
        assert!(g.check(None, RiskLevel::Medium).is_err());
    }

    #[test]
    fn rate_limiter_does_not_cap_low_risk() {
        let g = SafetyGuard::with_mode(ApprovalMode::FullAuto);
        // `Low` risk bypasses the limiter. We don't have a sleep
        // here, but the relevant assertion is that we can record
        // many `Low` actions in a row.
        for _ in 0..(DEFAULT_RATE_BURST * 5) {
            g.check(None, RiskLevel::Low).expect("low risk exempt");
        }
        assert_eq!(g.hit_count(), 0);
    }

    #[test]
    fn check_rejects_blacklisted_chord() {
        let g = SafetyGuard::new();
        assert!(matches!(
            g.check(Some("ctrl+alt+delete"), RiskLevel::Medium),
            Err(CuaError::Denied(_))
        ));
    }

    #[test]
    fn normalise_chord_sorts_modifier_order() {
        assert_eq!(normalise_chord("b+a"), "a+b");
        assert_eq!(normalise_chord("Shift+Ctrl+T"), "ctrl+shift+t");
        assert_eq!(normalise_chord("  ctrl + a  "), "a+ctrl");
    }
}