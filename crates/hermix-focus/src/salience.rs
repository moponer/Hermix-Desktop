//! Salience scoring — combined recency × frequency × pin score.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 ("salience 重排") +
//! §五 Phase 8 (FocusStack + salience 打分).
//!
//! ## Algorithm
//!
//! For a focus frame with `touch_count` touches, `pin_count` pins, and
//! age `t` (seconds since last touch), the salience is:
//!
//! ```text
//! salience = w_recency  * recency_score(t, half_life)
//!          + w_freq     * freq_score(touch_count)
//!          + w_pin      * pin_score(pin_count)
//! ```
//!
//! All three component scores are in `[0, 1]`. The weights default to
//! `0.5 / 0.3 / 0.2` (recency dominates, as the调研 recommended).
//!
//! - `recency_score(t) = 0.5 ^ (t / half_life)` — exponential decay with
//!   configurable half-life (default 24h).
//! - `freq_score(n) = 1 - 1 / (1 + ln(1 + n) / ln(10))` — log-saturating so
//!   extremely-frequently-touched frames don't drown out everything.
//! - `pin_score(p) = 1 - (1 / 2)^p` — saturates at 1 after one pin, but
//!   additional pins still register (for the dashboard).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalienceConfig {
    pub recency_weight: f32,
    pub freq_weight: f32,
    pub pin_weight: f32,
    /// Half-life of the recency decay, in seconds. Default 24h.
    pub half_life_secs: f64,
}

impl Default for SalienceConfig {
    fn default() -> Self {
        Self {
            recency_weight: 0.5,
            freq_weight: 0.3,
            pin_weight: 0.2,
            half_life_secs: 24.0 * 3600.0,
        }
    }
}

/// The three component scores that combine into the final salience.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SalienceComponents {
    pub recency: f32,
    pub freq: f32,
    pub pin: f32,
    pub total: f32,
}

/// Compute the salience of a frame.
///
/// - `last_touch`: when the frame was last touched (UTC)
/// - `touch_count`: how many times it has been touched total
/// - `pin_count`: how many times it has been pinned total
/// - `now`: reference time (pass `Utc::now()` in production; tests pass a
///   fixed time for determinism)
pub fn salience(
    last_touch: DateTime<Utc>,
    touch_count: u32,
    pin_count: u32,
    now: DateTime<Utc>,
    cfg: &SalienceConfig,
) -> SalienceComponents {
    let age_secs = (now - last_touch).num_milliseconds() as f64 / 1000.0;
    let age_secs = age_secs.max(0.0);
    let recency = recency_score(age_secs, cfg.half_life_secs) as f32;
    let freq = freq_score(touch_count) as f32;
    let pin = pin_score(pin_count) as f32;
    let total = (recency * cfg.recency_weight
        + freq * cfg.freq_weight
        + pin * cfg.pin_weight)
        .clamp(0.0, 1.0);
    SalienceComponents {
        recency,
        freq,
        pin,
        total,
    }
}

pub fn recency_score(age_secs: f64, half_life_secs: f64) -> f64 {
    if half_life_secs <= 0.0 {
        return 0.0;
    }
    0.5_f64.powf(age_secs / half_life_secs)
}

pub fn freq_score(touch_count: u32) -> f64 {
    if touch_count == 0 {
        return 0.0;
    }
    let n = touch_count as f64;
    1.0 - 1.0 / (1.0 + (1.0 + n).ln() / 10.0_f64.ln())
}

pub fn pin_score(pin_count: u32) -> f64 {
    if pin_count == 0 {
        return 0.0;
    }
    1.0 - 0.5_f64.powi(pin_count as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t_minus(secs: i64) -> DateTime<Utc> {
        Utc::now() - Duration::seconds(secs)
    }

    #[test]
    fn recency_at_zero_is_one() {
        assert!((recency_score(0.0, 3600.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn recency_at_half_life_is_half() {
        let h = 3600.0;
        let s = recency_score(h, h);
        assert!((s - 0.5).abs() < 1e-9, "expected 0.5, got {s}");
    }

    #[test]
    fn recency_decays_monotonically() {
        let h = 3600.0;
        let mut last = 1.0;
        for secs in (0..24).map(|i| i as f64 * 600.0) {
            let s = recency_score(secs, h);
            assert!(s <= last + 1e-9);
            last = s;
        }
    }

    #[test]
    fn recency_zero_half_life_returns_zero() {
        assert_eq!(recency_score(100.0, 0.0), 0.0);
    }

    #[test]
    fn freq_zero_is_zero() {
        assert_eq!(freq_score(0), 0.0);
    }

    #[test]
    fn freq_grows_then_saturates() {
        let f1 = freq_score(1);
        let f10 = freq_score(10);
        let f100 = freq_score(100);
        let f1000 = freq_score(1000);
        assert!(f1 < f10);
        assert!(f10 < f100);
        assert!((f1000 - f100).abs() < 0.1, "freq should saturate");
    }

    #[test]
    fn pin_zero_is_zero() {
        assert_eq!(pin_score(0), 0.0);
    }

    #[test]
    fn pin_one_is_half() {
        assert!((pin_score(1) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn pin_saturates_to_one() {
        // 10 pins is essentially 1.0.
        assert!(pin_score(10) > 0.999);
    }

    #[test]
    fn salience_combines_weights() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let s = salience(now, 5, 1, now, &cfg);
        // Just-touched frame with 5 touches and 1 pin: all components high.
        assert!(s.total > 0.7, "fresh frame should be salient: {s:?}");
    }

    #[test]
    fn salience_decays_with_age() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let fresh = salience(now, 5, 1, now, &cfg);
        let old = salience(t_minus(7 * 24 * 3600), 5, 1, now, &cfg);
        assert!(fresh.total > old.total, "fresh should beat old");
    }

    #[test]
    fn salience_pin_dominates_age() {
        // A pinned-but-old frame should still be moderately salient.
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let old_unpinned = salience(t_minus(7 * 24 * 3600), 1, 0, now, &cfg);
        let old_pinned = salience(t_minus(7 * 24 * 3600), 1, 1, now, &cfg);
        assert!(old_pinned.total > old_unpinned.total);
    }

    #[test]
    fn salience_total_in_unit_range() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        for days in 0..30 {
            for touches in 0..5 {
                for pins in 0..3 {
                    let t = now - Duration::days(days);
                    let s = salience(t, touches, pins, now, &cfg);
                    assert!((0.0..=1.0).contains(&s.total), "out of range: {s:?}");
                }
            }
        }
    }
}
