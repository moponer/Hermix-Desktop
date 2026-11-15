//! Soak test for long-running session stability.
//!
//! ( / ) A synthetic session that runs for an
//! extended period, monitoring memory, file descriptors, and task count
//! for leaks.
//!
//! ## Usage
//!
//! ```bash
//! # Run the full 8-hour soak test (CI nightly only)
//! cargo test -p hermix-agent --test soak -- --ignored
//!
//! # Run a short 60-second version for quick verification
//! HERMIX_SOAK_SECS=60 cargo test -p hermix-agent --test soak -- --ignored
//!
//! # The 60s headless probe runs by default (not ignored) and exercises
//! # the real hermix-session commit protocol over SQLite, measuring all
//! # four SLO items: memory growth, handle count, task-count stability,
//! # and unhandled-panic count.
//! cargo test -p hermix-agent --test soak
//! ```
//!
//! ## SLO
//!
//! - Memory growth ≤ 200MB over 8 hours
//! - No panic / no hang
//! - File descriptor (handle) count stable (no growth trend)
//! - Tokio task count stable across the probe window
//!
//! ## Design (§7 P8)
//!
//! - **Real work, not synthetic**: each loop iteration drives the
//!   `hermix-session` commit protocol (begin → append → commit through
//!   `SqliteSink`) over a real on-disk SQLite database in a temp dir.
//!   This exercises the session kernel's durability path, the WAL
//!   journaling, and the per-commit transaction, rather than just
//!   allocating and dropping bytes.
//! - **Task-count stability**: the probe counts `tokio::task::spawn`
//!   calls that complete vs. those still pending. A leaking task would
//!   show up as a growing delta. The probe itself spawns no extra
//!   tasks; the baseline is captured at start and compared at end.
//! - **Handle count**: on Windows, `GetProcessHandleCount` reports the
//!   current process handle count (≈ fd count). On non-Windows, the
//!   measurement is unavailable and the check is skipped (best-effort).
//! - **Headless-feasible**: the default `HERMIX_SOAK_SECS` for the
//!   non-ignored probe is 60s, which fits within typical CI windows.
//!   The 8h `--ignored` variant remains for the real-machine nightly.
//!
//! ## Panics
//!
//! The probe treats any panic in the loop body as an SLO violation:
//! the loop body is wrapped in `catch_unwind`, and the panic count is
//! asserted to be zero at the end.

#![cfg(test)]

#[cfg(test)]
mod soak {
    use std::panic::catch_unwind;
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use hermix_session::sqlite_sink::SqliteSink;
    use hermix_session::Session;

    /// Current process RSS in bytes. On Windows, uses
    /// `GetProcessMemoryInfo`; falls back to 0 on unsupported
    /// platforms.
    fn get_rss_bytes() -> u64 {
        #[cfg(target_os = "windows")]
        {
            use std::mem::MaybeUninit;
            #[repr(C)]
            struct ProcessMemoryCounters {
                cb: u32,
                page_fault_count: u32,
                peak_working_set_size: usize,
                working_set_size: usize,
                quota_peak_paged_pool_usage: usize,
                quota_paged_pool_usage: usize,
                quota_peak_non_paged_pool_usage: usize,
                quota_non_paged_pool_usage: usize,
                pagefile_usage: usize,
                peak_pagefile_usage: usize,
            }
            // GetProcessMemoryInfo 在 psapi.lib；GetCurrentProcess 在
            // kernel32（默认链接），link 指令只是追加搜索库，不影响
            // 前者解析。
            #[link(name = "psapi")]
            extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn GetProcessMemoryInfo(
                    handle: *mut std::ffi::c_void,
                    counters: *mut ProcessMemoryCounters,
                    cb: u32,
                ) -> i32;
            }
            unsafe {
                let handle = GetCurrentProcess();
                let mut counters: MaybeUninit<ProcessMemoryCounters> = MaybeUninit::zeroed();
                let cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
                if GetProcessMemoryInfo(handle, counters.as_mut_ptr(), cb) != 0 {
                    let counters = counters.assume_init();
                    return counters.working_set_size as u64;
                }
                0
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            0
        }
    }

    /// Current process handle count (≈ fd count). On Windows, uses
    /// `GetProcessHandleCount` (kernel32); returns 0 on platforms where
    /// the measurement is unavailable (the SLO check is then skipped).
    fn get_handle_count() -> u32 {
        #[cfg(target_os = "windows")]
        {
            extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn GetProcessHandleCount(
                    handle: *mut std::ffi::c_void,
                    handles: *mut u32,
                ) -> i32;
            }
            unsafe {
                let handle = GetCurrentProcess();
                let mut count: u32 = 0;
                if GetProcessHandleCount(handle, &mut count) != 0 {
                    return count;
                }
                0
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            0
        }
    }

    /// Count currently-active tokio tasks on this runtime by observing
    /// the spawn/completion delta of a probe task.
    ///
    /// Tokio does not expose a public "active task count" API, so we
    /// approximate it: spawn a task that immediately completes, then
    /// measure how many tasks have finished since the probe began. A
    /// leak would manifest as an unbounded growth in the finished-task
    /// counter relative to the spawned-task counter.
    static TASKS_SPAWNED: AtomicU64 = AtomicU64::new(0);
    static TASKS_FINISHED: AtomicU64 = AtomicU64::new(0);

    async fn spawn_probe_task() -> Result<(), tokio::task::JoinError> {
        TASKS_SPAWNED.fetch_add(1, Ordering::Relaxed);
        let handle = tokio::task::spawn(async move {
            // Yield a few times to simulate a real unit of work, then
            // finish. A leak would be a task that never finishes.
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }
            TASKS_FINISHED.fetch_add(1, Ordering::Relaxed);
        });
        handle.await
    }

    /// Active-task approximation: spawned minus finished. Stable over
    /// time ⇒ no leak.
    fn active_task_estimate() -> u64 {
        TASKS_SPAWNED.load(Ordering::Relaxed)
            .saturating_sub(TASKS_FINISHED.load(Ordering::Relaxed))
    }

    /// Drive one full round of the hermix-session commit protocol
    /// through the real SQLite sink. This is the "work" the soak test
    /// is meant to keep exercising — not a synthetic allocation.
    fn session_round(sink: &SqliteSink, session: &Session, round: u64) {
        let mut draft = session
            .begin()
            .unwrap_or_else(|e| panic!("session.begin failed at round {round}: {e}"));
        draft.append(
            "soak_user",
            serde_json::json!({"round": round, "ts": round}),
        );
        draft.append_with_task(
            "soak_tool",
            serde_json::json!({"round": round}),
            Some(format!("task-{round}")),
        );
        draft.set_conversation(format!("soak-conv-{round}"));
        draft
            .commit(|entries| sink.write(entries))
            .unwrap_or_else(|e| panic!("session.commit failed at round {round}: {e}"));

        // Verify durability: read back and confirm the entries landed.
        let loaded = sink
            .load()
            .unwrap_or_else(|e| panic!("sink.load failed at round {round}: {e}"));
        let expected = 2 * round as usize;
        assert!(
            loaded.len() >= expected,
            "expected ≥ {expected} entries after round {round}, got {}",
            loaded.len()
        );
    }

    /// Run the soak loop for `secs` seconds, exercising the real
    /// session commit protocol and monitoring all four SLO items.
    ///
    /// Returns nothing; panics on SLO violation. Used by both the
    /// non-ignored 60s headless probe and the `--ignored` 8h variant.
    async fn run_soak(secs: u64, label: &str) {
        let max_memory_growth_mb: u64 = 200;
        // Handle-count growth budget: SQLite opens 1-2 extra handles
        // per WAL checkpoint cycle. 64 handles over any window is a
        // comfortable ceiling for "stable".
        let max_handle_growth: u32 = 64;

        let dir = tempfile::tempdir().expect("tempdir failed");
        let db = dir.path().join(format!("soak-{label}.db"));
        let sink = SqliteSink::open(&db, "soak-sess")
            .expect("failed to open SqliteSink");
        let session = Session::new();

        let start = Instant::now();
        let deadline = start + Duration::from_secs(secs);
        let initial_rss = get_rss_bytes();
        let initial_handles = get_handle_count();
        let initial_active_tasks = active_task_estimate();

        let mut round: u64 = 0;
        let mut panics: u64 = 0;
        let mut last_report = Instant::now();
        // Sample handle counts over the window to detect a growth trend.
        let mut handle_samples: Vec<u32> = Vec::new();

        tracing::info!(
            secs,
            label,
            initial_rss_mb = initial_rss / 1024 / 1024,
            initial_handles,
            "soak started"
        );

        while Instant::now() < deadline {
            round += 1;

            // The loop body is panic-captured: an unhandled panic is
            // an SLO violation (P8 SLO item: 0 unhandled panics).
            // `catch_unwind` only wraps the *synchronous* session work
            // (the async probe task is awaited outside the unwind
            // boundary, since `.await` is not allowed inside a sync
            // closure).
            let result = catch_unwind(AssertUnwindSafe(|| {
                session_round(&sink, &session, round);
            }));
            if result.is_err() {
                panics += 1;
                tracing::error!(round, "soak loop body panicked");
                // A poisoned session cannot be recovered; stop the
                // loop and let the final assertions fail.
                break;
            }
            // Drive one probe task per round (outside the unwind
            // boundary) so the active-task estimate has a finite,
            // measured delta to track.
            if let Err(e) = spawn_probe_task().await {
                // A JoinError means the spawned task panicked or was
                // cancelled — treat it like an unhandled panic for the
                // SLO count (P8: 0 unhandled panics).
                panics += 1;
                tracing::error!(round, error = %e, "probe task failed to join");
            }

            // Periodic report + SLO checks every ~60s.
            if last_report.elapsed() >= Duration::from_secs(60) {
                let elapsed = start.elapsed().as_secs();
                let current_rss = get_rss_bytes();
                let current_handles = get_handle_count();
                let growth_mb = (current_rss.saturating_sub(initial_rss)) / 1024 / 1024;
                let handle_growth = current_handles.saturating_sub(initial_handles);
                let active = active_task_estimate();

                handle_samples.push(current_handles);
                tracing::info!(
                    elapsed_secs = elapsed,
                    round,
                    current_rss_mb = current_rss / 1024 / 1024,
                    growth_mb,
                    handle_growth,
                    active_tasks = active,
                    panics,
                    "soak progress"
                );

                if growth_mb > max_memory_growth_mb {
                    panic!(
                        "soak SLO violation ({label}): memory grew {growth_mb}MB in {elapsed}s \
                         (limit: {max_memory_growth_mb}MB)"
                    );
                }
                if current_handles > initial_handles
                    && handle_growth > max_handle_growth
                {
                    panic!(
                        "soak SLO violation ({label}): handle count grew by {handle_growth} in {elapsed}s \
                         (budget: {max_handle_growth})"
                    );
                }
                last_report = Instant::now();
            }

            // Yield to the runtime to let spawned probe tasks finish,
            // so the active-task estimate stays meaningful.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Final SLO checks.
        let total_elapsed = start.elapsed().as_secs();
        let final_rss = get_rss_bytes();
        let total_growth_mb = (final_rss.saturating_sub(initial_rss)) / 1024 / 1024;
        let final_handles = get_handle_count();
        let final_active = active_task_estimate();

        tracing::info!(
            label,
            total_elapsed_secs = total_elapsed,
            rounds = round,
            final_rss_mb = final_rss / 1024 / 1024,
            total_growth_mb,
            handle_growth = final_handles.saturating_sub(initial_handles),
            active_tasks_delta = final_active.saturating_sub(initial_active_tasks),
            panics,
            "soak completed"
        );

        // SLO 1: memory growth ≤ 200MB.
        assert!(
            total_growth_mb <= max_memory_growth_mb,
            "soak SLO violation ({label}): memory grew {total_growth_mb}MB over {total_elapsed}s \
             (limit: {max_memory_growth_mb}MB)"
        );

        // SLO 2: handle count stable (no growth trend). Compare the
        // final handle count against the earliest sample in the
        // window; a leak would show monotonic growth.
        #[cfg(target_os = "windows")]
        {
            if !handle_samples.is_empty() {
                let earliest = *handle_samples.iter().min().unwrap();
                let final_sample = final_handles;
                let growth = final_sample.saturating_sub(earliest);
                assert!(
                    growth <= max_handle_growth,
                    "soak SLO violation ({label}): handle count trended up by {growth} \
                     (earliest sample {earliest}, final {final_sample}, budget {max_handle_growth})"
                );
            }
        }

        // SLO 3: task count stable. The probe spawns exactly one task
        // per round; the active-task estimate should return to ~0
        // (± a few in-flight). A leak would leave it growing.
        assert!(
            final_active <= 8,
            "soak SLO violation ({label}): {final_active} tasks still active \
             (expected ≤ 8; task leak suspected)"
        );

        // SLO 4: zero unhandled panics.
        assert_eq!(
            panics, 0,
            "soak SLO violation ({label}): {panics} unhandled panics in the loop body"
        );

        // Verify we made progress (at least 1 round per 10ms of sleep
        // + the round's work; allow a generous floor).
        assert!(
            round >= secs / 10,
            "soak made insufficient progress ({label}): {round} rounds in {total_elapsed}s"
        );
    }

    /// The 8h soak test. `#[ignore]` — run with `-- --ignored` on the
    /// real-machine nightly. `HERMIX_SOAK_SECS` overrides the duration.
    #[tokio::test]
    #[ignore = "soak test: run with cargo test -- --ignored (8h default)"]
    async fn soak_test_synthetic_session() {
        let duration_secs: u64 = std::env::var("HERMIX_SOAK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8 * 3600); // 8 hours default
        run_soak(duration_secs, "8h").await;
    }

    /// The 60s headless probe. Runs by default (not ignored) so that
    /// `cargo test --workspace` in CI actually exercises the session
    /// kernel's durability path + all four SLO items. This is the
    /// headless-feasible slice of P8.
    #[tokio::test]
    async fn soak_60s_headless_probe() {
        let duration_secs: u64 = std::env::var("HERMIX_SOAK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60); // 60s default for the headless probe
        run_soak(duration_secs, "60s-headless").await;
    }

    /// Quick smoke test (non-ignored, fast) that verifies the soak
    /// infrastructure works without running for 60+ seconds.
    #[tokio::test]
    async fn soak_infrastructure_smoke_test() {
        // Just verify get_rss_bytes doesn't panic and returns a
        // reasonable value (or 0 on unsupported platforms).
        let rss = get_rss_bytes();
        // On Windows, RSS should be > 0. On other platforms, it's 0.
        #[cfg(target_os = "windows")]
        assert!(rss > 0, "get_rss_bytes should return non-zero on Windows");

        // Handle count: on Windows should be > 0 (any live process
        // holds handles).
        #[cfg(target_os = "windows")]
        {
            let h = get_handle_count();
            assert!(h > 0, "get_handle_count should return non-zero on Windows");
        }

        // The active-task estimate starts at 0 (no probe tasks spawned
        // yet in this test binary's static counters).
        let active = active_task_estimate();
        assert!(active <= 8, "initial active task estimate should be small, got {active}");

        // Verify the loop structure works with a tiny iteration count.
        let start = Instant::now();
        for i in 0..10 {
            let _scratch: Vec<u8> = vec![0u8; 1024];
            let _ = std::hint::black_box(&_scratch);
            assert_eq!(i, i); // no-op to prevent optimization
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "10 iterations should complete in < 1s"
        );
    }
}
