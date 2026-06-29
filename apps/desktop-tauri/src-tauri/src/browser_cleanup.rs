//! Best-effort Chrome process cleanup at Tauri shutdown.
//!
//! When the agent (via `crates/hermix-tools/src/tools/cdp_bootstrap.rs`)
//! needs a CDP endpoint and no Chrome is already listening, it spawns
//! `msedge.exe` / `chrome.exe` with `--remote-debugging-port=9222` and
//! then **detaches** the `Child` handle. The OS keeps the browser
//! running for as long as the user keeps using it, which is what we
//! want — but if hermix-desktop.exe exits first (user closes the
//! window, quits, OS kills the app, …) the browser becomes an
//! **orphan** squatting on `127.0.0.1:9222`:
//!
//! * The next hermix launch hits `probe_cdp()` and sees the port is
//!   "AlreadyRunning", so it never spawns a fresh browser. The
//!   surviving instance still works in the happy case, but if it was
//!   a leftover from a crash, the cookies/profile may be stale.
//! * If the user later installs a different Chrome for unrelated
//!   reasons and that one tries to bind 9222, it fails with
//!   `EADDRINUSE` and the agent loses browser tools.
//!
//! Fix: persist the PID to `<hermix_home>/runtime/browser.pid` at
//! spawn time (the bootstrap side does this), then on Tauri
//! `WindowEvent::Destroyed` taskkill that PID. The file is the only
//! handle we have, by design — keeping the `Child` would tie browser
//! lifecycle to the sidecar, but the browser is supposed to outlive
//! any single command.
//!
//! # Why duplicate `crates/hermix-tools/src/tools/cdp_bootstrap.rs::kill_spawned_chrome`?
//!
//! That function lives inside `hermix-tools`, which pulls in
//! `rusqlite`, `lopdf`, `xcap`, `enigo`, … — far too heavy for the
//! Tauri shell. We re-implement the four-step cleanup here so the
//! Tauri binary doesn't pay for the tool crate's transitive
//! dependencies. The path-resolution rules stay in lock-step with
//! `hermix_config::paths::hermix_home()`:
//!
//! 1. `$HERMIX_PORTABLE_DATA_DIR` (set by `portable_env` in dev mode)
//! 2. `$HERMIX_HOME` (explicit override)
//! 3. `~/.hermix` (default)
//!
//! If any of the resolution steps or the taskkill semantics ever
//! change in `cdp_bootstrap.rs`, mirror them here. The tests at the
//! bottom pin the contract.

use std::path::PathBuf;
use std::process::Stdio;

/// Filename of the PID file written by the CDP bootstrap.
const BROWSER_PID_FILENAME: &str = "browser.pid";

/// Resolve the hermix data dir using the same precedence as
/// `hermix_config::paths::hermix_home()`. Returns `None` only when
/// neither `HERMIX_PORTABLE_DATA_DIR` / `HERMIX_HOME` is set and we
/// cannot determine the user home (e.g. a stripped-down CI container).
fn resolve_data_dir() -> Option<PathBuf> {
    // 1. Portable-desktop override (highest priority).
    if let Some(v) = std::env::var_os("HERMIX_PORTABLE_DATA_DIR") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    // 2. Explicit HERMIX_HOME.
    if let Some(v) = std::env::var_os("HERMIX_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    // 3. Default: $HOME/.hermix (Linux/macOS) or %USERPROFILE%/.hermix
    //    (Windows). `dirs::home_dir()` covers both via the `directories`
    //    crate, which we already depend on transitively through `dirs`.
    dirs::home_dir().map(|h| h.join(".hermix"))
}

/// Compute `<data>/runtime/browser.pid`. Returns `None` if we cannot
/// resolve the data dir — caller treats that as a silent no-op
/// (there's no PID file to clean up in that case anyway).
fn browser_pid_path() -> Option<PathBuf> {
    resolve_data_dir().map(|d| d.join("runtime").join(BROWSER_PID_FILENAME))
}

/// Kill the Chrome process the CDP bootstrap previously spawned,
/// identified by `<hermix_home>/runtime/browser.pid`.
///
/// Idempotent and best-effort: every step short-circuits on failure
/// (no PID file → no-op; stale or missing PID → remove file and
/// return; taskkill non-zero exit → log at debug and remove file
/// anyway). The goal is "try once, leave the system clean", not
/// "guarantee the process is dead" — if the PID file is corrupted
/// and the real browser is still alive, the next `probe_cdp()` will
/// find it on `9222` and the user can manually stop it via Task
/// Manager.
///
/// Safe to call from `on_window_event` (synchronous). Safe to call
/// from a hot path: the function does at most one file read, one
/// `taskkill` invocation, and one file delete — worst case ~50ms.
pub fn kill_spawned_chrome() {
    let Some(pid_path) = browser_pid_path() else {
        return;
    };
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        // No PID file → the bootstrap never ran in this session, OR
        // it already cleaned up after a previous shutdown. Either way
        // nothing to do.
        Err(_) => return,
    };
    let pid = match pid_str.trim().parse::<u32>() {
        Ok(n) if n > 0 => n,
        _ => {
            eprintln!(
                "[Hermix:browser_cleanup] browser.pid contains invalid content {pid_str:?} — removing it"
            );
            let _ = std::fs::remove_file(&pid_path);
            return;
        }
    };
    // /F = force, /T = include child processes (renderer/GPU/network
    // service). Without /T, Chrome on Windows frequently leaves the
    // renderer alive and the next probe sees 9222 still bound.
    let result = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match result {
        Ok(status) if status.success() => {
            eprintln!(
                "[Hermix:browser_cleanup] Cleaned up orphan Chrome (PID {pid}) on hermix-desktop shutdown"
            );
        }
        Ok(status) => {
            // Non-zero usually means "process already gone" — that's
            // a successful cleanup from our perspective.
            eprintln!(
                "[Hermix:browser_cleanup] taskkill for Chrome (PID {pid}) returned {status} — process may already be gone"
            );
        }
        Err(e) => {
            eprintln!(
                "[Hermix:browser_cleanup] Could not invoke taskkill for Chrome (PID {pid}): {e}"
            );
        }
    }
    // Always remove the PID file so the next launch sees a clean
    // slate. If taskkill failed for a real reason (e.g. permissions),
    // the next launch will still see "port 9222 occupied" via
    // `probe_cdp()` and surface that as an explicit error to the
    // user, which is preferable to a stale PID file silently keeping
    // us from re-spawning.
    let _ = std::fs::remove_file(&pid_path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `HERMIX_PORTABLE_DATA_DIR` / `HERMIX_HOME` are process-global
    /// state; concurrent tests reading/writing them race. Lock the
    /// entire test body to keep them serialised.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Save the three relevant env vars so we can restore them after
    /// each test (we must not trample the developer's shell).
    fn save_env() -> [Option<String>; 3] {
        [
            std::env::var("HERMIX_PORTABLE_DATA_DIR").ok(),
            std::env::var("HERMIX_HOME").ok(),
            std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok(),
        ]
    }
    fn restore_env(saved: [Option<String>; 3]) {
        let keys = ["HERMIX_PORTABLE_DATA_DIR", "HERMIX_HOME"];
        for (k, v) in keys.iter().zip(saved.iter()) {
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        // HOME / USERPROFILE restored below if we mutated them.
    }

    /// `resolve_data_dir` follows the documented precedence:
    /// `HERMIX_PORTABLE_DATA_DIR` > `HERMIX_HOME` > `~/.hermix`.
    #[test]
    fn resolve_data_dir_precedence() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = save_env();
        unsafe {
            std::env::remove_var("HERMIX_PORTABLE_DATA_DIR");
            std::env::remove_var("HERMIX_HOME");
        }

        // 1. PORTABLE wins over HOME.
        let tmp = tempfile::tempdir().unwrap();
        let portable = tmp.path().join("portable");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&portable).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", &portable);
            std::env::set_var("HERMIX_HOME", &home);
        }
        assert_eq!(resolve_data_dir(), Some(portable.clone()));

        // 2. HOME wins over the default.
        unsafe {
            std::env::remove_var("HERMIX_PORTABLE_DATA_DIR");
        }
        assert_eq!(resolve_data_dir(), Some(home.clone()));

        // 3. Empty PORTABLE is treated as unset (shells sometimes
        //    set "" by accident).
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", "");
        }
        assert_eq!(resolve_data_dir(), Some(home));

        restore_env(saved);
    }

    /// No PID file → function is a no-op (no panic, no taskkill).
    #[test]
    fn kill_spawned_chrome_no_pid_file_is_noop() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = save_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", tmp.path());
        }
        // Pre-condition: no `runtime/browser.pid`.
        assert!(!tmp.path().join("runtime").join("browser.pid").exists());
        // Should not panic, should not create any files.
        kill_spawned_chrome();
        assert!(!tmp.path().join("runtime").exists());
        restore_env(saved);
    }

    /// A PID file containing `0` (or any non-numeric content) is
    /// treated as invalid — the file is removed and we exit cleanly
    /// without invoking taskkill.
    #[test]
    fn kill_spawned_chrome_invalid_pid_clears_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = save_env();
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let pid_path = runtime.join("browser.pid");
        std::fs::write(&pid_path, "not-a-number").unwrap();
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", tmp.path());
        }
        kill_spawned_chrome();
        // File removed; nothing else created.
        assert!(!pid_path.exists());
        restore_env(saved);
    }

    /// A PID file containing `0` is treated as invalid — the file is
    /// removed and we exit cleanly.
    #[test]
    fn kill_spawned_chrome_zero_pid_clears_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = save_env();
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let pid_path = runtime.join("browser.pid");
        std::fs::write(&pid_path, "0").unwrap();
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", tmp.path());
        }
        kill_spawned_chrome();
        assert!(!pid_path.exists());
        restore_env(saved);
    }

    /// A PID file pointing at a definitely-dead process (PID 4 is
    /// `System` on Windows and never killable by us, so taskkill
    /// returns non-zero; but on non-Windows we still shell out and
    /// it also fails to kill a fake PID). The function must still
    /// remove the PID file regardless of taskkill's exit code —
    /// otherwise we'd accumulate stale PIDs forever.
    #[test]
    fn kill_spawned_chrome_removes_pid_file_even_when_taskkill_fails() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = save_env();
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let pid_path = runtime.join("browser.pid");
        // PID 1 on Linux is `init`/systemd; on Windows `taskkill`
        // would refuse to kill System. Either way taskkill should
        // exit non-zero on a normal user account, exercising the
        // "process may already be gone" branch.
        std::fs::write(&pid_path, "1").unwrap();
        unsafe {
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", tmp.path());
        }
        kill_spawned_chrome();
        // PID file removed regardless of taskkill result.
        assert!(!pid_path.exists(), "PID file must be removed even when taskkill fails");
        restore_env(saved);
    }
}