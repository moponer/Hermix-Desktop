//! Data directory strategy for the desktop app.
//!
//! Two categories of data, two locations:
//!
//! | Category | What | Where | Why |
//! |----------|------|-------|-----|
//! | **User data** | config.yaml, .env, auth.json, sessions.db, memory_store.db, enabled_toolsets.json | `%APPDATA%/Hermix Desktop/data` | Survives reinstalls, clean C: drive, user-owned |
//! | **App transient** | WebView2 cache, logs | `<install_root>/webview-cache`, `<install_root>/logs` | Stateless, auto-regenerated, can be deleted safely |
//!
//! The `HERMIX_PORTABLE_DATA_DIR` env var is set by this module to
//! the user-data path so the backend sidecar (`hermix.exe`) reads
//! config/sessions from the right place via `hermix_home()`.
//!
//! In dev mode, the user-data path is overridden to `hermix-data/`
//! at the workspace root so `pnpm tauri dev` shares state with
//! standalone `cargo run`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Has `install_portable_env` already run in this process?
///
/// The first call has full side effects (env-var injection, subdir
/// creation); subsequent calls are no-ops. We use an `AtomicBool`
/// rather than a `OnceLock<()>` because we want lock-free fast path
/// and we want the second call to be observable from tests as a true
/// no-op (no env-var write happens, no subdir check happens).
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Compute the install root of the running desktop app.
///
/// The install root is the directory that contains the running `.exe`.
/// In release mode this is what the Tauri bundler produced (e.g.
/// `D:\Apps\Hermix Desktop\`). In dev mode (`pnpm tauri dev`) this
/// is the `target/debug/` directory, which is fine — dev mode is
/// not the portable scenario anyway.
pub fn compute_install_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot determine current_exe: {e}"))?;
    let parent = exe
        .parent()
        .ok_or_else(|| format!("current_exe {} has no parent dir", exe.display()))?;
    Ok(parent.to_path_buf())
}

/// Return the portable data dir if set by the Tauri launcher, else None.
///
/// This is the lower-level accessor for `HERMIX_PORTABLE_DATA_DIR`.
/// The Tauri launcher uses this to know where to create the initial
/// `data/` subdirectory tree before `hermix_home()` is ever called by
/// the spawned sidecar process. Most code paths should call
/// `install_portable_env()` (which sets this env var) rather than
/// reading it directly.
pub fn portable_data_dir() -> Option<PathBuf> {
    std::env::var("HERMIX_PORTABLE_DATA_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Install the portable-mode env vars. Idempotent.
pub fn install_portable_env() -> Result<PathBuf, String> {
    if INSTALLED.load(Ordering::Acquire) {
        return compute_install_root();
    }

    let install_root = compute_install_root()?;

    // Transient subdirs: always next to the exe.
    let webview_dir = install_root.join("webview-cache");
    let logs_dir = install_root.join("logs");

    for d in [&webview_dir, &logs_dir] {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("[Hermix] warning: cannot create {}: {e}", d.display());
        }
    }

    // Env vars for the backend sidecar.
    //
    // Dev mode: `find_dev_data_dir` now always returns None — the
    //   sidecar uses `~/.hermix/` (same as production). The old
    //   behavior redirected to `<workspace_root>/hermix-data` which
    //   scattered session data across two locations and made the
    //   unified C-drive path setup impossible.
    //
    // Production: DO NOT set HERMIX_PORTABLE_DATA_DIR/HERMIX_HOME.
    //   The sidecar's hermix_home() defaults to ~/.hermix/ (user
    //   home, C drive). No benefit to overriding it — doing so
    //   would require migrating old data.
    if let Some(dev_dir) = find_dev_data_dir(&install_root) {
        set_if_unset("HERMIX_PORTABLE_DATA_DIR", &dev_dir);
        set_if_unset("HERMIX_HOME", &dev_dir);
    }
    set_if_unset("WEBVIEW2_USER_DATA_FOLDER", &webview_dir);
    set_if_unset("TMP", &logs_dir);
    set_if_unset("TEMP", &logs_dir);

    INSTALLED.store(true, Ordering::Release);
    Ok(install_root)
}

/// Detect whether the current process is the actual Tauri desktop app
/// binary (as opposed to a unit-test runner or doc-test harness).
///
/// Only the real `hermix-desktop.exe` should activate dev-mode data-dir
/// detection; test binaries should use the standard `install_root/data`
/// layout so their assertions about subdir existence and env-var values
/// remain valid.
///
/// Heuristic: check the current exe basename. The real app's .exe is
/// `hermix-desktop.exe` (or `hermix_desktop.exe` on some platforms);
/// test harnesses have names like
/// `hermix_desktop_lib-a544c4018afccca5.exe` or `cargo_test.exe`.
pub fn is_tauri_app_binary() -> bool {
    let exe = std::env::current_exe().ok();
    let basename = exe
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    basename == "hermix-desktop" || basename == "hermix_desktop"
}

/// Formerly returned `<workspace_root>/hermix-data` when it existed
/// and contained `sessions.db`. This caused session data to scatter
/// across two locations (workspace hermix-data/ vs user ~/.hermix/data/)
/// and made the unified C-drive path setup impossible.
///
/// Now always returns None — dev mode uses the same `~/.hermix/` as
/// production. The user who already unified their data to the C drive
/// must set `HERMIX_HOME` or `HERMIX_PORTABLE_DATA_DIR` explicitly if
/// they want a non-default location.
fn find_dev_data_dir(_install_root: &Path) -> Option<PathBuf> {
    None
}

/// Test-only: reset the idempotency flag so a subsequent test can
/// re-run the first-time side effects.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    INSTALLED.store(false, Ordering::Release);
}

/// Set `key=value` only if the key is currently unset.
///
/// We use `set_if_unset` (rather than unconditional `set_var`) so a
/// developer or test harness that has already set these env vars
/// (e.g. to point at a sandbox) keeps control. In a normal bundled
/// install no one has set these yet, so all six are injected.
fn set_if_unset(key: &str, value: &Path) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

/// Return the six portable env vars as a vector of `(key, value)`
/// pairs, suitable for passing to `Command::env` when spawning the
/// `hermix.exe` sidecar.
pub fn current_portable_env() -> Vec<(&'static str, std::ffi::OsString)> {
    let install_root = compute_install_root().unwrap_or_else(|_| PathBuf::from("."));
    let logs_dir = install_root.join("logs");
    let webview_dir = install_root.join("webview-cache");

    let mut out: Vec<(&'static str, std::ffi::OsString)> = Vec::with_capacity(4);
    // Only forward HERMIX_PORTABLE_DATA_DIR/HERMIX_HOME if the parent
    // (install_portable_env) already set them (i.e. dev mode with
    // hermix-data/). In production they are NOT set, and we must NOT
    // inject a fallback — the sidecar should use its default ~/.hermix/.
    let _ = std::env::var_os("HERMIX_PORTABLE_DATA_DIR").map(|v| {
        out.push(("HERMIX_PORTABLE_DATA_DIR", v));
    });
    let _ = std::env::var_os("HERMIX_HOME").map(|v| {
        out.push(("HERMIX_HOME", v));
    });
    push_if_present(&mut out, "WEBVIEW2_USER_DATA_FOLDER", || webview_dir.as_os_str().to_owned());
    push_if_present(&mut out, "TMP", || logs_dir.as_os_str().to_owned());
    push_if_present(&mut out, "TEMP", || logs_dir.as_os_str().to_owned());
    out
}

fn push_if_present<F>(
    out: &mut Vec<(&'static str, std::ffi::OsString)>,
    key: &'static str,
    default: F,
) where
    F: FnOnce() -> std::ffi::OsString,
{
    let val = std::env::var_os(key).unwrap_or_else(default);
    out.push((key, val));
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env_lock<F: FnOnce() -> R, R>(f: F) -> R {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    #[test]
    fn compute_install_root_returns_absolute_path() {
        let root = compute_install_root().expect("compute_install_root should succeed");
        assert!(!root.as_os_str().is_empty());
        assert!(root.is_absolute(), "install_root must be absolute, got: {}", root.display());
    }

    #[test]
    fn install_portable_env_is_idempotent() {
        with_env_lock(|| {
            let saved: Vec<(&str, Option<String>)> = [
                "HERMIX_HOME", "HERMIX_PORTABLE_DATA_DIR",
                "HERMIX_PORTABLE_CACHE_DIR", "WEBVIEW2_USER_DATA_FOLDER",
                "TMP", "TEMP",
            ].iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for (k, _) in &saved { std::env::remove_var(k); }
            reset_for_tests();

            let sentinel = "Z:\\sentinel\\first\\call\\should\\not\\overwrite";
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", sentinel);
            let r1 = install_portable_env();
            let after_first = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();

            let new_sentinel = "Z:\\sentinel\\second\\call\\should\\not\\touch";
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", new_sentinel);
            let r2 = install_portable_env();
            let after_second = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();

            for (k, v) in &saved {
                match v { Some(val) => std::env::set_var(k, val), None => std::env::remove_var(k), }
            }
            reset_for_tests();

            assert!(r1.is_ok());
            assert!(r2.is_ok());
            assert_eq!(r1.unwrap(), r2.unwrap());
            assert_eq!(after_first.as_deref(), Some(sentinel));
            assert_eq!(after_second.as_deref(), Some(new_sentinel));
        });
    }

    #[test]
    fn install_portable_env_creates_subdirs() {
        with_env_lock(|| {
            for k in ["HERMIX_HOME","HERMIX_PORTABLE_DATA_DIR","HERMIX_PORTABLE_CACHE_DIR",
                       "WEBVIEW2_USER_DATA_FOLDER","TMP","TEMP"] {
                std::env::remove_var(k);
            }
            reset_for_tests();
            let root = install_portable_env().expect("install_portable_env should succeed");
            for sub in ["data", "cache", "logs", "webview-cache"] {
                let p = root.join(sub);
                assert!(p.is_dir(), "expected subdir: {} (root: {})", p.display(), root.display());
            }
        });
    }

    #[test]
    fn install_portable_env_sets_core_vars() {
        with_env_lock(|| {
            let saved: Vec<(&str, Option<String>)> = [
                "WEBVIEW2_USER_DATA_FOLDER", "TMP", "TEMP",
            ].iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for (k, _) in &saved { std::env::remove_var(k); }
            reset_for_tests();
            let _root = install_portable_env().expect("install_portable_env should succeed");
            let after: Vec<String> = saved.iter().map(|(k, v)| {
                let cur = std::env::var(k).ok();
                match v { Some(val) => std::env::set_var(k,val), None => std::env::remove_var(k), }
                cur.unwrap_or_default()
            }).collect();
            let mut idx = 0;
            for (k, _) in &saved {
                let val = after[idx].clone(); idx += 1;
                assert!(!val.is_empty(), "env var {k} should be set");
                let p = PathBuf::from(&val);
                assert!(p.is_absolute(), "{k} must be absolute, got: {val}");
            }
        });
    }

    #[test]
    fn current_portable_env_returns_core_vars() {
        let vars = current_portable_env();
        // WEBVIEW2_USER_DATA_FOLDER, TMP, TEMP are always present.
        // HERMIX_PORTABLE_DATA_DIR/HERMIX_HOME only if set in env.
        assert!(vars.len() >= 3, "expected at least 3 core vars, got {}", vars.len());
        // Check WEBVIEW2_USER_DATA_FOLDER is present (highest priority).
        assert!(vars.iter().any(|(k,_)| *k == "WEBVIEW2_USER_DATA_FOLDER"),
            "WEBVIEW2_USER_DATA_FOLDER must be present");
        assert!(vars.iter().any(|(k,_)| *k == "TMP"),
            "TMP must be present");
        assert!(vars.iter().any(|(k,_)| *k == "TEMP"),
            "TEMP must be present");
    }

    #[test]
    fn current_portable_env_preserves_existing_var() {
        with_env_lock(|| {
            let sentinel = std::ffi::OsString::from("Z:\\sandbox\\portable\\data");
            let saved = std::env::var_os("HERMIX_PORTABLE_DATA_DIR");
            std::env::set_var("HERMIX_PORTABLE_DATA_DIR", &sentinel);
            let vars = current_portable_env();
            let actual = vars.iter().find(|(k,_)| *k == "HERMIX_PORTABLE_DATA_DIR")
                .map(|(_,v)| v.clone()).expect("should be present");
            match saved { Some(v) => std::env::set_var("HERMIX_PORTABLE_DATA_DIR", v), None => std::env::remove_var("HERMIX_PORTABLE_DATA_DIR"), }
            assert_eq!(actual, sentinel);
        });
    }
}
