//! TTL cache wrapper for [`ToolRegistry`](crate::ToolRegistry) `check_fn` closures.
//!
//! ## Why this exists
//!
//! Every `ToolEntry` in the registry has a `check_fn: Arc<dyn Fn() -> bool + Send + Sync>` that
//! is called on the hot path of [`crate::ToolRegistry::get_definitions`] and
//! [`crate::ToolRegistry::is_available`]. For "always true" checks
//! (`Arc::new(|| true)`) the closure is free. But for **environment
//! probes** — "is `OPENAI_API_KEY` set?", "is the browser driver
//! running?", "is the MCP server reachable?" — re-running the probe on
//! every LLM turn is wasteful and can introduce latency.
//!
//! `cached_check` wraps a slow `check_fn` in a small per-entry
//! `Mutex<(Option<Instant>, bool)>` that holds the result for
//! [`DEFAULT_TTL`] (30 seconds). Within the TTL window the wrapper
//! returns the cached `bool` without touching the inner closure; after
//! the window elapses the next call re-runs the probe and refreshes
//! the cache.
//!
//! ## Trade-off
//!
//! 30 s is the **lifetime** of an LLM turn. If a user adds an env var
//! mid-conversation, the affected tool will appear stale-unavailable
//! for up to 30 s before the next probe. That's the cost of avoiding
//! network/env I/O on every turn; for a desktop assistant it's a
//! perfectly acceptable lag.
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use hermix_tools::cached_check::{cached_check, cached_check_default};
//!
//! let probe = Arc::new(|| std::env::var("OPENAI_API_KEY").is_ok());
//! let cached = cached_check_default(probe); // 30 s TTL
//! registry.register_with_risk(
//!     "vision_analyze",
//!     "vision",
//!     schema,
//!     handler,
//!     cached,                 // wraps the probe
//!     vec!["OPENAI_API_KEY".into()],
//!     true,
//!     "Vision tool",
//!     "👁",
//!     None,
//!     RiskLevel::Safe,
//!     /* override_existing = */ false,
//! );
//! ```

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Type alias for the closure shape that `ToolRegistry` expects.
pub type CheckFn = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// Default TTL for [`cached_check_default`]. 30 s ≈ the lifetime of a
/// single LLM turn; the next turn re-probes if the env changed.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Wrap `check_fn` in a TTL cache. Within `ttl` of the previous probe,
/// returns the cached result without re-running the closure. After
/// `ttl` elapses, the next call re-runs the closure and refreshes the
/// cache.
///
/// The cache is **per wrapper instance** — i.e. per `Arc` returned from
/// this function. Different registrations should call `cached_check`
/// separately to keep their caches independent; sharing a wrapper
/// between two tools means they share a single cache entry, which is
/// rarely what you want for unrelated probes.
pub fn cached_check(check_fn: CheckFn, ttl: Duration) -> CheckFn {
    let cache: std::sync::Arc<Mutex<(Option<Instant>, bool)>> =
        std::sync::Arc::new(Mutex::new((None, false)));
    std::sync::Arc::new(move || {
        let mut slot = cache.lock().expect("cached_check mutex poisoned");
        let now = Instant::now();
        if let Some(ts) = slot.0 {
            if now.duration_since(ts) < ttl {
                return slot.1;
            }
        }
        let result = check_fn();
        *slot = (Some(now), result);
        result
    })
}

/// Convenience: [`cached_check`] with the default 30 s TTL.
pub fn cached_check_default(check_fn: CheckFn) -> CheckFn {
    cached_check(check_fn, DEFAULT_TTL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Cached check returns the cached value within the TTL window
    /// without re-running the underlying probe.
    #[test]
    fn test_cached_check_returns_cached_within_ttl() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_inner = runs.clone();
        let probe: CheckFn = Arc::new(move || {
            runs_inner.fetch_add(1, Ordering::SeqCst);
            true
        });
        let cached = cached_check(probe, Duration::from_millis(500));

        // First call probes + caches.
        assert!(cached());
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Next 100 calls within the window are cache hits.
        for _ in 0..100 {
            assert!(cached());
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "must not re-probe within TTL");
    }

    /// After the TTL elapses, the next call re-probes and refreshes
    /// the cache.
    #[test]
    fn test_cached_check_reruns_after_ttl() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_inner = runs.clone();
        let probe: CheckFn = Arc::new(move || {
            runs_inner.fetch_add(1, Ordering::SeqCst);
            true
        });
        // 50 ms TTL so the test runs fast.
        let cached = cached_check(probe, Duration::from_millis(50));

        assert!(cached());
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Sleep past the TTL.
        std::thread::sleep(Duration::from_millis(60));

        assert!(cached());
        assert_eq!(runs.load(Ordering::SeqCst), 2, "must re-probe after TTL");
    }

    /// Cache records the latest value, not just the first. If the
    /// probe flips mid-window, the cache reflects the new state after
    /// the next re-probe.
    #[test]
    fn test_cached_check_tracks_latest_result() {
        // Start with `false`, flip to `true` after the TTL.
        let flag = Arc::new(AtomicUsize::new(0));
        let flag_inner = flag.clone();
        let probe: CheckFn = Arc::new(move || flag_inner.load(Ordering::SeqCst) > 0);
        let cached = cached_check(probe, Duration::from_millis(30));

        // Initially false.
        assert!(!cached());

        // Flip the underlying truth.
        flag.store(1, Ordering::SeqCst);

        // Still cached as false within the window.
        assert!(!cached());

        // After the window, next call re-probes and returns true.
        std::thread::sleep(Duration::from_millis(40));
        assert!(cached());
    }

    /// Two `cached_check` wrappers are independent — they don't share
    /// state. Useful to confirm that registering the same probe under
    /// two different tool names doesn't cross-contaminate.
    #[test]
    fn test_cached_check_independent_instances() {
        let runs_a = Arc::new(AtomicUsize::new(0));
        let runs_b = Arc::new(AtomicUsize::new(0));
        let a_inner = runs_a.clone();
        let b_inner = runs_b.clone();
        let a = cached_check(
            Arc::new(move || {
                a_inner.fetch_add(1, Ordering::SeqCst);
                true
            }),
            Duration::from_secs(30),
        );
        let b = cached_check(
            Arc::new(move || {
                b_inner.fetch_add(1, Ordering::SeqCst);
                false
            }),
            Duration::from_secs(30),
        );

        assert!(a());
        assert!(!b());
        assert_eq!(runs_a.load(Ordering::SeqCst), 1);
        assert_eq!(runs_b.load(Ordering::SeqCst), 1);
    }

    /// `cached_check_default` uses the documented 30 s TTL.
    #[test]
    fn test_default_ttl_is_30_seconds() {
        assert_eq!(DEFAULT_TTL, Duration::from_secs(30));
    }
}
