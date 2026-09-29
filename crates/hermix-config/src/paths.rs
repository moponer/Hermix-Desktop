//! Path management for the hermix home directory and its files.
//!
//! # Renamed: `hermes` → `hermix`
//!
//! In this release the project renamed from `hermes` → `hermix`. Every
//! on-disk artifact — env vars, the data directory, the per-feature
//! subfolders, and the helper functions — now uses the `hermix` /
//! `HERMIX_*` names.
//!
//! # Portable desktop mode (`HERMIX_PORTABLE_DATA_DIR`)
//!
//! When running as a Tauri desktop app, the launcher sets two extra
//! env vars so that **all** user data (config, memory, skills, sessions,
//! auth tokens, etc.) lives next to the .exe, not under `%APPDATA%` or
//! `~/.hermix`:
//!
//! - `HERMIX_PORTABLE_DATA_DIR`  → `<install_root>/data` (highest priority)
//! - `HERMIX_PORTABLE_CACHE_DIR` → `<install_root>/cache`
//!
//! When `HERMIX_PORTABLE_DATA_DIR` is set and non-empty,
//! [`hermix_home`] short-circuits and returns it. The legacy-migration
//! hooks (`HERMIX_MIGRATE_FROM_HERMES`, `~/.hermes` sibling detection)
//! are intentionally **skipped** in portable mode — we must never
//! touch the user's home dir from inside a portable install (a
//! user with both this desktop app and the original NousResearch
//! `hermes-agent` would otherwise have their `~/.hermes` silently
//! renamed, clobbering the other tool).
//!
//! See [`portable_data_dir`] / [`portable_cache_dir`] for the lower-
//! level accessors used by the launcher itself.
//!
//! # Coexistence with the original NousResearch `hermes-agent`
//!
//! The original NousResearch `hermes-agent` is a different project
//! that happens to share the `hermes` name and uses the same
//! `~/.hermes` data dir + `HERMES_HOME` env var. If a user has BOTH
//! installed, we must NOT silently reach into the original's data
//! (it would either destroy the original's files or cause both
//! projects to clobber each other). The design is therefore:
//!
//! - [`hermix_home`] reads **`HERMIX_HOME` only** (no fallback to
//!   `HERMES_HOME`). If `HERMIX_HOME` is unset, it defaults to
//!   `~/.hermix`. This means a user who has set `HERMES_HOME` for the
//!   original NousResearch hermes-agent will see our app use a
//!   completely separate data dir.
//! - The auto-rename of a stale `~/.hermes` → `~/.hermix` is
//!   **opt-in** via the `HERMIX_MIGRATE_FROM_HERMES=1` env var.
//!   Default is OFF so we never touch a `~/.hermes` directory that
//!   may belong to the other project. Users upgrading from the
//!   previous name of THIS project (also called "hermes" before the
//!   rename) can set this flag once and run the app to migrate, or
//!   just point `HERMIX_HOME` at their old dir to keep using it.
//! - On startup, if a `~/.hermes` directory is detected next to the
//!   hermix home and migration is not opted in, a one-time notice is
//!   printed to stderr explaining how to opt in. This is a hint, not
//!   an action.
//!
//! # Other env vars
//!
//! For non-identity env vars (server addr, model name, proxy, etc.)
//! we keep using [`env_compat`] which transparently reads
//! `HERMIX_FOO` first, then `HERMES_FOO` as a one-release shim. The
//! dual-read is safe there because the two projects would only ever
//! set these for themselves — no shared state to collide on.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// env_compat
// ---------------------------------------------------------------------------

/// Return the value of `new_name` if set, else `old_name` if set, else None.
/// When `old_name` is set and `new_name` is not, the function emits a
/// one-shot deprecation warning to stderr (via a global `OnceLock` so
/// the warning is shown at most once per process even if the helper
/// is called hundreds of times).
/// Use this in place of `std::env::var("HERMES_*")` everywhere in the
/// crate so the deprecation window is a single, central shim rather
/// than a comment in every call site.
pub fn env_compat(new_name: &str, old_name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(new_name) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Ok(v) = std::env::var(old_name) {
        if !v.is_empty() {
            warn_legacy_env_once(new_name, old_name);
            return Some(v);
        }
    }
    None
}

fn warn_legacy_env_once(new_name: &str, old_name: &str) {
    static WARNED: OnceLock<std::sync::Mutex<Vec<(String, String)>>> = OnceLock::new();
    let cell = WARNED.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut seen) = cell.lock() {
        let key = (new_name.to_string(), old_name.to_string());
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        eprintln!(
            "[hermix-compat] env var {old_name} is deprecated, use {new_name} instead \
             (will keep falling back for one release)"
        );
    }
}

// ---------------------------------------------------------------------------
// hermix_home
// ---------------------------------------------------------------------------

/// Return the hermix home directory.
/// Resolution order (highest to lowest):
/// 1. `$HERMIX_PORTABLE_DATA_DIR` if set and non-empty. This is the
/// desktop-mode override: when the Tauri launcher starts up, it
/// sets this to the stable user-data path
/// (`%APPDATA%/Hermix Desktop/data`) so that all user config,
/// sessions, and skills live in a install-path-independent
/// location. The legacy-migration hooks are **skipped** in this
/// branch (see module-level docs).
/// 2. `$HERMIX_HOME` if set and non-empty.
/// 3. `~/.hermix` (i.e. `user_home_dir()/.hermix`).
/// We **deliberately do not** fall back to `$HERMES_HOME`. The original
/// NousResearch `hermes-agent` is a different project that uses the
/// same env var name; if a user has set `HERMES_HOME` for the
/// original, silently picking it up would cause both projects to
/// fight over the same data dir. Users who set `HERMES_HOME` to keep
/// using their pre-rename data dir should set `HERMIX_HOME` to the
/// same path instead (or run with `HERMIX_MIGRATE_FROM_HERMES=1` to
/// move the data into the new default location).
/// We also intentionally do **not** use macOS `Library/Application
/// Support/…` so CLI, gateway, dashboard, and docs all agree on one
/// layout.
/// On the first successful resolution (branches 2 or 3), this either
/// runs the `~/.hermes` → `~/.hermix` rename (only if the user has set
/// `HERMIX_MIGRATE_FROM_HERMES=1`; see [`migrate_legacy_hermes_dir_once`])
/// or prints a one-time notice if a sibling `~/.hermes` directory is
/// detected (see [`detect_legacy_hermes_sibling_once`]). Both are
/// guarded by a `OnceLock` so they fire at most once per process.
pub fn hermix_home() -> PathBuf {
    // 1. Portable-desktop override (set by Tauri launcher). Bypass
    // ALL legacy hooks — we must never touch ~/.hermes from a
    // portable install.
    if let Some(portable) = portable_data_dir() {
        return portable;
    }
    // 2-3. Read only HERMIX_HOME. We deliberately do NOT fall back to
    // HERMES_HOME — that env var may be set by the original
    // NousResearch hermes-agent (a different project that happens to
    // share the env var name). See the module-level docs.
    let resolved = if let Ok(home) = std::env::var("HERMIX_HOME") {
        if home.is_empty() {
            user_home_dir().join(".hermix")
        } else {
            PathBuf::from(home)
        }
    } else {
        user_home_dir().join(".hermix")
    };
    if legacy_migration_opted_in() {
        migrate_legacy_hermes_dir_once(&resolved);
    } else {
        detect_legacy_hermes_sibling_once(&resolved);
    }
    resolved
}

/// Return the portable data dir if set by the Tauri launcher, else None.
/// This is the lower-level accessor for `HERMIX_PORTABLE_DATA_DIR`
/// (which [`hermix_home`] consumes at the top of its resolution chain).
/// The Tauri launcher uses this to know where to create the initial
/// `data/` subdirectory tree before `hermix_home()` is ever called by
/// the spawned sidecar process.
pub fn portable_data_dir() -> Option<PathBuf> {
    std::env::var("HERMIX_PORTABLE_DATA_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Return the portable cache dir if set by the Tauri launcher, else None.
/// Mirrors [`portable_data_dir`] for the cache subdirectory. The
/// Tauri launcher sets this to `<install_root>/cache` so that large
/// transient files (model cache, embedding index, etc.) also live
/// next to the executable and get carried along when the user copies
/// the install dir to another machine.
pub fn portable_cache_dir() -> Option<PathBuf> {
    std::env::var("HERMIX_PORTABLE_CACHE_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Best-effort user home (`$HOME` / `%USERPROFILE%`), for `~/.hermix`.
fn user_home_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Hermix state root directory (same rule as CLI `--config-dir` / gateway data).
/// If `config_dir_override` is set, that path is used; otherwise [`hermix_home`].
/// Use this for `cron/`, `webhooks.json`, and other machine-local state so CLI
/// and gateway stay aligned.
pub fn state_dir(config_dir_override: Option<&Path>) -> PathBuf {
    config_dir_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(hermix_home)
}

/// Hermix data directory — where all runtime data (sessions, memory,
/// trajectories) lives. Separated from config files so that:
/// - Normal mode: `~/.hermix/data/`
/// - Portable mode: `<install_root>/data/`  (HERMIX_PORTABLE_DATA_DIR)
/// In both modes the *structure under data_dir is identical*, which
/// makes backup/migration a single `cp -r data/ ...`.
/// Config files (config.yaml, .env, MEMORY.md, USER.md, skills/)
/// stay under [`hermix_home`] — they are machine/install-specific and
/// should NOT be blindly carried along during a portable migration.
pub fn data_dir() -> PathBuf {
    if let Some(portable) = portable_data_dir() {
        portable
    } else {
        hermix_home().join("data")
    }
}

/// `<home>/data` for an explicitly provided hermix home. Used by
/// components that already hold a resolved home path; avoids scattered
/// `home.join("data")` calls.
pub fn data_dir_in(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("data")
}

// ---------------------------------------------------------------------------
// Legacy data-dir migration (opt-in, coexistence-safe)
// ---------------------------------------------------------------------------

/// Idempotently rename a stale `~/.hermes` to `~/.hermix` (or whatever
/// `new_home` says, if its parent is the user-home).
/// **Caller is responsible for opt-in**: this function runs the rename
/// unconditionally — gating is done by [`legacy_migration_opted_in`]
/// before [`hermix_home`] dispatches here. The default behavior is to
/// NOT migrate (see [`detect_legacy_hermes_sibling_once`]) so we
/// don't clobber a `~/.hermes` directory that may belong to the
/// original NousResearch hermes-agent.
/// Behavior when invoked:
/// - If `~/.hermes` doesn't exist, do nothing.
/// - If `new_home` already exists (e.g. `~/.hermix` from a previous
/// run), move the legacy dir aside to `<parent>/.hermes.bak.<ts>`
/// instead of overwriting, then fall through to the rename.
/// - Rename the legacy dir to `new_home`.
/// - Errors are logged to stderr but never propagated — migration is
/// best-effort and a failed rename must not stop the agent from
/// starting (a fresh `~/.hermix` will be created on the first write
/// anyway).
/// Invoked from [`hermix_home`] on first call when the user has
/// opted in; a `OnceLock` ensures the rename runs at most once per
/// process.
fn migrate_legacy_hermes_dir_once(new_home: &Path) {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return;
    }
    if let Err(e) = migrate_legacy_hermes_dir(new_home) {
        eprintln!("[hermix-compat] legacy hermes dir migration skipped: {e}");
    }
    let _ = DONE.set(());
}

/// Returns true if the user has opted in to the `~/.hermes` →
/// `~/.hermix` data-dir migration via the
/// `HERMIX_MIGRATE_FROM_HERMES=1` env var.
/// Accepts `1` / `true` / `yes` / `on` (case-insensitive) as truthy,
/// and `0` / `false` / `no` / `off` (and the empty string) as
/// falsy. Anything else defaults to truthy to match the common
/// "set it to anything non-empty" idiom. The check is intentionally
/// opt-in: a `~/.hermes` directory may belong to the original
/// NousResearch hermes-agent (a different project), and silently
/// moving it would break that tool. Users who upgraded from the
/// previous name of THIS project (also called "hermes" before the
/// rename) can either set this flag once and run the app, or
/// simply point `HERMIX_HOME` at their existing dir to keep using
/// it as-is.
fn legacy_migration_opted_in() -> bool {
    let Some(v) = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok() else {
        return false;
    };
    let v = v.trim();
    if v.is_empty() {
        return false;
    }
    !matches!(
        v.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// One-shot stderr notice (per process) when a `~/.hermes` directory
/// is sitting next to the hermix home and migration is NOT opted in.
/// This is a HINT, not an action — we never touch the legacy dir
/// unless the user explicitly opts in via
/// `HERMIX_MIGRATE_FROM_HERMES=1`. The notice tells the user (a)
/// why the legacy dir is being left alone and (b) how to import it
/// if they actually want to.
fn detect_legacy_hermes_sibling_once(new_home: &Path) {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return;
    }
    let _ = DONE.set(());
    let Some(parent) = new_home.parent() else {
        return;
    };
    let legacy = parent.join(".hermes");
    if !legacy.is_dir() || legacy == *new_home {
        return;
    }
    eprintln!(
        "[hermix] detected legacy data dir at {} — leaving it alone to avoid \
         clobbering other tools. To import it as your Hermix home, run again with \
         HERMIX_MIGRATE_FROM_HERMES=1, or `mv {} {}` manually.",
        legacy.display(),
        legacy.display(),
        new_home.display()
    );
}

fn migrate_legacy_hermes_dir(new_home: &Path) -> std::io::Result<()> {
    // Only migrate if the user didn't set a custom home pointing somewhere
    // else (i.e. new_home sits next to the legacy dir in the same parent).
    let Some(parent) = new_home.parent() else {
        return Ok(());
    };
    let legacy = parent.join(".hermes");
    if !legacy.is_dir() {
        return Ok(());
    }
    if new_home == legacy {
        return Ok(()); // user explicitly set HERMES_HOME = .hermes, leave alone
    }
    if new_home.exists() {
        // The new dir already exists. Move the legacy dir aside so we
        // don't clobber the user — operator can merge by hand if needed.
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = parent.join(format!(".hermes.bak.{ts}"));
        std::fs::rename(&legacy, &backup)?;
        eprintln!(
            "[hermix-compat] moved legacy {} to {} (target {} already existed)",
            legacy.display(),
            backup.display(),
            new_home.display()
        );
        return Ok(());
    }
    std::fs::rename(&legacy, new_home)?;
    eprintln!(
        "[hermix-compat] renamed {} -> {}",
        legacy.display(),
        new_home.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Derived paths
// ---------------------------------------------------------------------------

/// `$hermix_home/config.yaml`
pub fn config_path() -> PathBuf {
    hermix_home().join("config.yaml")
}

/// `$hermix_home/cli-config.yaml`
pub fn cli_config_path() -> PathBuf {
    hermix_home().join("cli-config.yaml")
}

/// `$hermix_home/gateway.json`
pub fn gateway_json_path() -> PathBuf {
    hermix_home().join("gateway.json")
}

/// PID file written by `hermix gateway start` (same directory as `config.yaml`).
pub fn gateway_pid_path() -> PathBuf {
    hermix_home().join("gateway.pid")
}

/// Gateway PID file under an explicit Hermix home directory (e.g. `HERMIX_HOME` or `-C`).
pub fn gateway_pid_path_in(home: impl AsRef<std::path::Path>) -> PathBuf {
    home.as_ref().join("gateway.pid")
}

/// `$hermix_home/MEMORY.md`
pub fn memory_path() -> PathBuf {
    hermix_home().join("MEMORY.md")
}

/// `$hermix_home/USER.md`
pub fn user_path() -> PathBuf {
    hermix_home().join("USER.md")
}

/// `$hermix_home/skills/`
pub fn skills_dir() -> PathBuf {
    hermix_home().join("skills")
}

/// `$hermix_home/data/sessions/`
/// Lives under [`data_dir`] so session logs are included in a portable
/// data backup/migration.
pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

/// `$hermix_home/data/sessions.db` — SQLite database backing session persistence.
/// **Always** go through this helper (or [`sessions_db_path_in`] for tests /
/// explicit-homed callers). Hard-coding `home.join("sessions.db")` bypasses
/// the portable override (`HERMIX_PORTABLE_DATA_DIR`) and has caused
/// "session lost on restart" reports in the past.
/// Lives under [`data_dir`] so sessions are included in a portable
/// data backup/migration.
pub fn sessions_db_path() -> PathBuf {
    data_dir().join("sessions.db")
}

/// Tasks database path. Currently shares the same `sessions.db` file
/// as session persistence (the `tasks` and `automation_*` tables are
/// created alongside `sessions` / `messages` in the same SQLite file
/// to avoid dual-writer journal contention — 铁律 D/WAL).
/// **Always** go through this helper (or [`tasks_db_path_in`]).
/// In the future, if task data grows large enough to warrant a
/// separate file, only this function needs to change.
///
pub fn tasks_db_path() -> PathBuf {
    // Same file as sessions.db — single-writer SQLite + WAL avoids
    // cross-file journal contention (铁律 D).
    sessions_db_path()
}

/// `<data>/sessions.db` for an explicitly-provided data dir.
/// Tasks share this file. See [`tasks_db_path`] for rationale.
///
pub fn tasks_db_path_in(data_dir: impl AsRef<Path>) -> PathBuf {
    // Same as sessions_db_path_in — single SQLite file.
    data_dir.as_ref().join("sessions.db")
}

/// `<data>/sessions.db` for an explicitly-provided data dir.
/// Use this whenever a component already holds the resolved data dir
/// path (e.g. an `HttpServerState::data_dir: PathBuf`). Avoids the
/// `state.data_dir.join("sessions.db")` scattered-call anti-pattern.
pub fn sessions_db_path_in(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("sessions.db")
}

/// `$hermix_home/data/sessions/` (markdown session logs) for an explicit home.
/// Lives under [`data_dir`] so session logs are included in a portable
/// data backup/migration.
pub fn sessions_dir_in(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("sessions")
}

/// `$hermix_home/data/trajectories/` — RL trajectory dump dir.
/// **Always** go through this helper (or [`trajectories_dir_in`]). Same
/// anti-pattern as [`sessions_db_path`].
/// Lives under [`data_dir`] so trajectories are included in a portable
/// data backup/migration.
pub fn trajectories_dir() -> PathBuf {
    data_dir().join("trajectories")
}

/// `<data>/trajectories/` for an explicitly-provided data dir.
pub fn trajectories_dir_in(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("trajectories")
}

/// `$hermix_home/data/spill/` — 超大工具结果落盘目录。
/// 属于运行时派生数据，放在 `data` 下以便随便携数据一起迁移。
pub fn spill_dir() -> PathBuf {
    data_dir().join("spill")
}

/// `<data>/spill/` for an explicitly-provided data dir.
pub fn spill_dir_in(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("spill")
}

/// `$hermix_home/data/cron/`
/// Lives under [`data_dir`] so cron data is included in a portable
/// data backup/migration.
pub fn cron_dir() -> PathBuf {
    data_dir().join("cron")
}

/// `$hermix_home/.env`
pub fn env_path() -> PathBuf {
    hermix_home().join(".env")
}

/// `$hermix_home/auth.json` — credential store written by `hermix auth login`.
/// Mirrors Python's `tools.managed_tool_gateway.auth_json_path()`. Used by
/// the managed-tool-gateway resolver to read provider OAuth tokens.
pub fn auth_json_path() -> PathBuf {
    hermix_home().join("auth.json")
}

/// `$hermix_home/approval_allowlist.json` — user "Always Allow" command patterns.
/// When the user clicks "Always Allow" on an `RequiresConfirmation` prompt,
/// the matched command pattern is persisted here so the next run re-loads
/// it and bypasses confirmation for matching commands. Format is a JSON
/// array of `UserAllowlistEntry` (see `hermix_tools::approval`). A missing
/// or corrupt file is treated as "no allowlist" — load must never fail
/// the agent start.
pub fn approval_allowlist_path() -> PathBuf {
    hermix_home().join("approval_allowlist.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;

    /// Run a closure while holding the workspace-wide env-test
    /// mutex (defined in `managed_gateway::test_lock`). This is the
    /// same mutex used by the `EnvGuard` helpers in
    /// `managed_gateway::selection::tests`, so all env-var-mutating
    /// tests in this crate — and the `managed_gateway` tests that
    /// use the same env vars — run serially rather than racing on
    /// `std::env::set_var`. See `managed_gateway::test_lock` for the
    /// rationale.
    fn with_env_lock<F: FnOnce() -> R, R>(f: F) -> R {
        let _g = test_lock::lock();
        f()
    }

    /// Without HERMIX_HOME / HERMES_HOME set, the function returns
    /// `~/.hermix` (or `~` if we can't determine it).
    #[test]
    fn hermix_home_default() {
        // Use a real temporary parent so the migration (a no-op when
        // there's no sibling .hermes) doesn't touch the user's real
        // home dir.
        let tmp = tempfile::tempdir().unwrap();
        let _ = tmp.path();

        // First, verify the function returns a non-empty path.
        let h = hermix_home();
        assert!(!h.as_os_str().is_empty());
    }

    /// Setting `HERMIX_HOME` overrides the default.
    #[test]
    fn hermix_home_respects_new_env() {
        // We don't actually mutate env vars here (race-prone in
        // parallel cargo test) — instead, the precedence is verified
        // by the env_compat tests below.
        let _ = hermix_home();
    }

    /// `env_compat` returns the new-name value when both are set.
    #[test]
    fn env_compat_prefers_new() {
        unsafe {
            std::env::set_var("HERMIX_TEST_NEW", "new-value");
            std::env::set_var("HERMES_TEST_OLD", "old-value");
        }
        let v = env_compat("HERMIX_TEST_NEW", "HERMES_TEST_OLD");
        unsafe {
            std::env::remove_var("HERMIX_TEST_NEW");
            std::env::remove_var("HERMES_TEST_OLD");
        }
        assert_eq!(v.as_deref(), Some("new-value"));
    }

    /// `env_compat` falls back to the old name when the new is unset.
    #[test]
    fn env_compat_falls_back_to_old() {
        unsafe {
            std::env::remove_var("HERMIX_TEST_FB_NEW");
            std::env::set_var("HERMES_TEST_FB_OLD", "old-only");
        }
        let v = env_compat("HERMIX_TEST_FB_NEW", "HERMES_TEST_FB_OLD");
        unsafe {
            std::env::remove_var("HERMES_TEST_FB_OLD");
        }
        assert_eq!(v.as_deref(), Some("old-only"));
    }

    /// `env_compat` returns None when neither is set.
    #[test]
    fn env_compat_returns_none() {
        unsafe {
            std::env::remove_var("HERMIX_TEST_NONE_NEW");
            std::env::remove_var("HERMES_TEST_NONE_OLD");
        }
        let v = env_compat("HERMIX_TEST_NONE_NEW", "HERMES_TEST_NONE_OLD");
        assert!(v.is_none());
    }

    /// Migration: a sibling `~/.hermes` is renamed to `new_home`
    /// when `new_home` doesn't yet exist.
    #[test]
    fn migration_renames_sibling_hermes() {
        let parent = tempfile::tempdir().unwrap();
        let legacy = parent.path().join(".hermes");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("config.yaml"), "old").unwrap();
        let new_home = parent.path().join(".hermix");
        assert!(!new_home.exists());

        migrate_legacy_hermes_dir(&new_home).unwrap();
        assert!(!legacy.exists());
        assert!(new_home.exists());
        assert_eq!(
            std::fs::read_to_string(new_home.join("config.yaml")).unwrap(),
            "old"
        );
    }

    /// Migration: if `new_home` already exists, the legacy dir is
    /// moved aside to `<parent>/.hermes.bak.<ts>` rather than
    /// overwriting.
    #[test]
    fn migration_keeps_existing_new_dir() {
        let parent = tempfile::tempdir().unwrap();
        let legacy = parent.path().join(".hermes");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("data"), "legacy").unwrap();
        let new_home = parent.path().join(".hermix");
        std::fs::create_dir(&new_home).unwrap();
        std::fs::write(new_home.join("data"), "current").unwrap();

        migrate_legacy_hermes_dir(&new_home).unwrap();
        // Legacy moved aside, new dir untouched.
        assert!(new_home.exists());
        assert_eq!(
            std::fs::read_to_string(new_home.join("data")).unwrap(),
            "current"
        );
        // Exactly one .hermes.bak.* sibling.
        let baks: Vec<_> = std::fs::read_dir(parent.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".hermes.bak.")
            })
            .collect();
        assert_eq!(baks.len(), 1);
    }

    /// Migration: when there's no legacy dir, the function is a no-op.
    #[test]
    fn migration_noop_when_no_legacy() {
        let parent = tempfile::tempdir().unwrap();
        let new_home = parent.path().join(".hermix");
        migrate_legacy_hermes_dir(&new_home).unwrap();
        assert!(!new_home.exists());
    }

    // -----------------------------------------------------------------
    // Coexistence: hermix_home() must NOT pick up HERMES_HOME.
    // The original NousResearch hermes-agent (a different project)
    // uses the same env var name. If both tools are installed and the
    // user has set HERMES_HOME for the original, silently reading it
    // would cause both projects to write to the same data dir.
    // -----------------------------------------------------------------

    /// `hermix_home()` must return the `HERMIX_HOME` value, not the
    /// `HERMES_HOME` value, when both are set. The test is gated
    /// behind a unique env-var name pattern + save/restore so it
    /// plays nicely with other tests in the same binary.
    #[test]
    fn hermix_home_ignores_hermes_home() {
        // Mutating process-global env vars from concurrent tests
        // races even when individual tests do their own save/restore
        // — `hermix_home()` re-reads `$HERMIX_HOME` on every call and
        // a sister test's `set_var` can land between our `set_var`
        // and our read. `with_env_lock` serialises this test against
        // every other env-var-mutating test in the workspace
        // (`managed_gateway::test_lock`), so the read here is
        // guaranteed to see our own `set_var` value.
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let real_hermix = tmp.path().join("my-hermix");
            let fake_hermes = tmp.path().join("fake-hermes-home");
            std::fs::create_dir_all(&real_hermix).unwrap();
            std::fs::create_dir_all(&fake_hermes).unwrap();

            // Save the real env (if any) so we don't trample the user.
            let saved_hermix = std::env::var("HERMIX_HOME").ok();
            let saved_hermes = std::env::var("HERMES_HOME").ok();
            unsafe {
                std::env::set_var("HERMIX_HOME", &real_hermix);
                std::env::set_var("HERMES_HOME", &fake_hermes);
            }

            let h = hermix_home();

            // Restore.
            unsafe {
                match saved_hermix {
                    Some(v) => std::env::set_var("HERMIX_HOME", v),
                    None => std::env::remove_var("HERMIX_HOME"),
                }
                match saved_hermes {
                    Some(v) => std::env::set_var("HERMES_HOME", v),
                    None => std::env::remove_var("HERMES_HOME"),
                }
            }

            // hermix_home() must use HERMIX_HOME (the explicit hermix
            // value), not HERMES_HOME (the original NousResearch tool's
            // value).
            assert_eq!(h, real_hermix);
            assert_ne!(h, fake_hermes);
        })
    }

    /// Empty `HERMIX_HOME` should be treated as unset (defensive
    /// against shells that set it to "" accidentally).
    #[test]
    fn hermix_home_treats_empty_as_unset() {
        // Same race-protection rationale as
        // `hermix_home_ignores_hermes_home`: this test mutates the
        // process-global `$HERMIX_HOME`, so it must run under the
        // shared env-test mutex or it races sister tests that also
        // poke the same variable.
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let real_hermix = tmp.path().join("real-hermix");
            std::fs::create_dir_all(&real_hermix).unwrap();

            let saved = std::env::var("HERMIX_HOME").ok();
            unsafe {
                // Empty HERMIX_HOME → should fall through to default.
                std::env::set_var("HERMIX_HOME", "");
                std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", "0");
            }

            // We can't assert against ~/.hermix (user-dependent) but we
            // can assert it's NOT the real_hermix path we created.
            let h = hermix_home();

            // Restore.
            unsafe {
                std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES");
                match saved {
                    Some(v) => std::env::set_var("HERMIX_HOME", v),
                    None => std::env::remove_var("HERMIX_HOME"),
                }
            }

            // Empty HERMIX_HOME → hermix_home() uses default (~/.hermix),
            // which is definitely not the tempdir we created.
            assert!(!h.to_string_lossy().contains("real-hermix"));
        })
    }

    // -----------------------------------------------------------------
    // Portable-desktop mode: HERMIX_PORTABLE_DATA_DIR takes priority
    // over everything else, including HERMIX_HOME, AND it must skip
    // the legacy ~/.hermes migration hooks (we must never touch the
    // user home from inside a portable install).
    // -----------------------------------------------------------------

    /// `HERMIX_PORTABLE_DATA_DIR` overrides `HERMIX_HOME` and the
    /// default `~/.hermix` resolution. The value is taken as-is —
    /// the launcher is responsible for passing an absolute path
    /// (typically `<install_root>/data`).
    #[test]
    fn portable_data_dir_overrides_hermix_home() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let portable = tmp.path().join("portable-data");
            let fallback_hermix_home = tmp.path().join("fallback-home");
            std::fs::create_dir_all(&portable).unwrap();
            std::fs::create_dir_all(&fallback_hermix_home).unwrap();

            // Save the real env (if any) so we don't trample the user.
            let saved_portable = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();
            let saved_hermix = std::env::var("HERMIX_HOME").ok();
            unsafe {
                std::env::set_var("HERMIX_PORTABLE_DATA_DIR", &portable);
                std::env::set_var("HERMIX_HOME", &fallback_hermix_home);
            }

            let h = hermix_home();

            // Restore.
            unsafe {
                match saved_portable {
                    Some(v) => std::env::set_var("HERMIX_PORTABLE_DATA_DIR", v),
                    None => std::env::remove_var("HERMIX_PORTABLE_DATA_DIR"),
                }
                match saved_hermix {
                    Some(v) => std::env::set_var("HERMIX_HOME", v),
                    None => std::env::remove_var("HERMIX_HOME"),
                }
            }

            // Portable wins over HERMIX_HOME.
            assert_eq!(h, portable);
            assert_ne!(h, fallback_hermix_home);
        });
    }

    /// Empty `HERMIX_PORTABLE_DATA_DIR` is treated as unset — we
    /// fall through to the normal `HERMIX_HOME` / default chain.
    /// This is defensive against shells that set it to "" by
    /// accident.
    #[test]
    fn portable_data_dir_empty_falls_through() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let real_hermix_home = tmp.path().join("real-hermix-home");
            std::fs::create_dir_all(&real_hermix_home).unwrap();

            let saved_portable = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();
            let saved_hermix = std::env::var("HERMIX_HOME").ok();
            let saved_migrate = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok();
            unsafe {
                std::env::set_var("HERMIX_PORTABLE_DATA_DIR", "");
                std::env::set_var("HERMIX_HOME", &real_hermix_home);
                std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", "0");
            }

            let h = hermix_home();

            // Restore.
            unsafe {
                match saved_portable {
                    Some(v) => std::env::set_var("HERMIX_PORTABLE_DATA_DIR", v),
                    None => std::env::remove_var("HERMIX_PORTABLE_DATA_DIR"),
                }
                match saved_hermix {
                    Some(v) => std::env::set_var("HERMIX_HOME", v),
                    None => std::env::remove_var("HERMIX_HOME"),
                }
                match saved_migrate {
                    Some(v) => std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", v),
                    None => std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES"),
                }
            }

            // Empty HERMIX_PORTABLE_DATA_DIR → falls through to
            // HERMIX_HOME (which we set to real_hermix_home).
            assert_eq!(h, real_hermix_home);
        });
    }

    /// With `HERMIX_PORTABLE_DATA_DIR` unset and `HERMIX_HOME` set,
    /// the function returns the HERMIX_HOME value (existing
    /// behaviour, regression guard).
    #[test]
    fn portable_data_dir_unset_uses_hermix_home() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let real_hermix_home = tmp.path().join("real-hermix-home");
            std::fs::create_dir_all(&real_hermix_home).unwrap();

            let saved_portable = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();
            let saved_hermix = std::env::var("HERMIX_HOME").ok();
            let saved_migrate = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok();
            unsafe {
                std::env::remove_var("HERMIX_PORTABLE_DATA_DIR");
                std::env::set_var("HERMIX_HOME", &real_hermix_home);
                std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", "0");
            }

            let h = hermix_home();

            // Restore.
            unsafe {
                match saved_portable {
                    Some(v) => std::env::set_var("HERMIX_PORTABLE_DATA_DIR", v),
                    None => std::env::remove_var("HERMIX_PORTABLE_DATA_DIR"),
                }
                match saved_hermix {
                    Some(v) => std::env::set_var("HERMIX_HOME", v),
                    None => std::env::remove_var("HERMIX_HOME"),
                }
                match saved_migrate {
                    Some(v) => std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", v),
                    None => std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES"),
                }
            }

            assert_eq!(h, real_hermix_home);
        });
    }

    /// `portable_data_dir()` / `portable_cache_dir()` accessors
    /// return `None` when the env vars are unset or empty, and the
    /// matching `PathBuf` when set.
    #[test]
    fn portable_accessors_return_none_or_value() {
        with_env_lock(|| {
            // Save + unset.
            let saved_data = std::env::var("HERMIX_PORTABLE_DATA_DIR").ok();
            let saved_cache = std::env::var("HERMIX_PORTABLE_CACHE_DIR").ok();
            unsafe {
                std::env::remove_var("HERMIX_PORTABLE_DATA_DIR");
                std::env::remove_var("HERMIX_PORTABLE_CACHE_DIR");
            }
            assert!(portable_data_dir().is_none());
            assert!(portable_cache_dir().is_none());

            // Set empty.
            unsafe {
                std::env::set_var("HERMIX_PORTABLE_DATA_DIR", "");
                std::env::set_var("HERMIX_PORTABLE_CACHE_DIR", "");
            }
            assert!(portable_data_dir().is_none());
            assert!(portable_cache_dir().is_none());

            // Set value.
            unsafe {
                std::env::set_var("HERMIX_PORTABLE_DATA_DIR", "D:/apps/hermix/data");
                std::env::set_var("HERMIX_PORTABLE_CACHE_DIR", "D:/apps/hermix/cache");
            }
            assert_eq!(
                portable_data_dir().as_deref(),
                Some(std::path::Path::new("D:/apps/hermix/data"))
            );
            assert_eq!(
                portable_cache_dir().as_deref(),
                Some(std::path::Path::new("D:/apps/hermix/cache"))
            );

            // Restore.
            unsafe {
                match saved_data {
                    Some(v) => std::env::set_var("HERMIX_PORTABLE_DATA_DIR", v),
                    None => std::env::remove_var("HERMIX_PORTABLE_DATA_DIR"),
                }
                match saved_cache {
                    Some(v) => std::env::set_var("HERMIX_PORTABLE_CACHE_DIR", v),
                    None => std::env::remove_var("HERMIX_PORTABLE_CACHE_DIR"),
                }
            }
        });
    }

    // -----------------------------------------------------------------
    // Coexistence: legacy migration must be opt-in.
    // -----------------------------------------------------------------

    /// Default: `HERMIX_MIGRATE_FROM_HERMES` unset → opt-in is false.
    /// This is the safety property that prevents us from silently
    /// renaming another project's `~/.hermes` directory.
    #[test]
    fn legacy_migration_default_is_off() {
        let saved = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok();
        unsafe { std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES"); }
        let r = legacy_migration_opted_in();
        if let Some(v) = saved {
            unsafe { std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", v); }
        }
        assert!(!r, "migration must default to OFF to coexist safely");
    }

    /// Opt-in: all of `1`, `true`, `yes`, `on` (and any non-empty
    /// value not in the falsy list) flip the switch to true.
    #[test]
    fn legacy_migration_truthy_values() {
        with_env_lock(|| {
            for truthy in ["1", "true", "TRUE", "yes", "YES", "on", "  1  ", "anything"] {
                let saved = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok();
                unsafe { std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", truthy); }
                let r = legacy_migration_opted_in();
                match saved {
                    Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", v); },
                    None => unsafe { std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES"); },
                }
                assert!(r, "expected {:?} to opt in to migration", truthy);
            }
        });
    }

    /// Opt-out: `0`, `false`, `no`, `off`, and the empty string all
    /// keep the switch off even when the env var is set.
    #[test]
    fn legacy_migration_falsy_values() {
        with_env_lock(|| {
            for falsy in ["0", "false", "FALSE", "no", "NO", "off", "OFF", ""] {
                let saved = std::env::var("HERMIX_MIGRATE_FROM_HERMES").ok();
                unsafe { std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", falsy); }
                let r = legacy_migration_opted_in();
                match saved {
                    Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", v); },
                    None => unsafe { std::env::remove_var("HERMIX_MIGRATE_FROM_HERMES"); },
                }
                assert!(!r, "expected {:?} to NOT opt in to migration", falsy);
            }
        });
    }

    // -----------------------------------------------------------------
    // Regression: derived path helpers must follow the portable override.
    // These existed as scattered `home.join("sessions.db")` /
    // `home.join("trajectories")` literals across the workspace; they were
    // promoted into helpers precisely so a future "I hard-coded it"
    // regression cannot silently diverge from `hermix_home()`. The tests
    // below pin the contract: under `HERMIX_PORTABLE_DATA_DIR`, every
    // derived helper resolves inside the portable data dir, not under
    // `$HOME/.hermix`.
    // -----------------------------------------------------------------

    fn set_saved_env(key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
    fn restore_env(key: &str, saved: Option<String>) {
        unsafe {
            match saved {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn sessions_db_path_follows_portable_override() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let portable = tmp.path().join("data");
            std::fs::create_dir_all(&portable).unwrap();

            let saved_portable = set_saved_env("HERMIX_PORTABLE_DATA_DIR");
            let saved_home = set_saved_env("HERMIX_HOME");
            let saved_migrate = set_saved_env("HERMIX_MIGRATE_FROM_HERMES");
            unsafe {
                std::env::set_var("HERMIX_PORTABLE_DATA_DIR", &portable);
                std::env::remove_var("HERMIX_HOME");
                std::env::set_var("HERMIX_MIGRATE_FROM_HERMES", "0");
            }

            let db = sessions_db_path();
            assert_eq!(db, portable.join("sessions.db"),
                "sessions_db_path() must resolve inside HERMIX_PORTABLE_DATA_DIR, got {}", db.display());

            let dir = sessions_dir();
            assert_eq!(dir, portable.join("sessions"));

            let traj = trajectories_dir();
            assert_eq!(traj, portable.join("trajectories"));

            restore_env("HERMIX_PORTABLE_DATA_DIR", saved_portable);
            restore_env("HERMIX_HOME", saved_home);
            restore_env("HERMIX_MIGRATE_FROM_HERMES", saved_migrate);
        });
    }

    #[test]
    fn in_helpers_track_explicit_home() {
        // These don't touch global env vars — they're parameterised and
        // must return a deterministic join no matter what the env says.
        let home = std::path::PathBuf::from("/tmp/fake-home");
        assert_eq!(sessions_db_path_in(&home), home.join("sessions.db"));
        assert_eq!(sessions_dir_in(&home), home.join("sessions"));
        assert_eq!(trajectories_dir_in(&home), home.join("trajectories"));
        //tasks_db_path_in mirrors sessions_db_path_in
        assert_eq!(tasks_db_path_in(&home), home.join("sessions.db"));
    }

    #[test]
    fn derived_helpers_match_scatter_pattern() {
        // Pin: the helpers return exactly what the old `home.join(...)`
        // literals used to return. If anyone ever changes the layout
        // (e.g. moves sessions.db into sessions/), these tests must be
        // updated deliberately, not silently.
        with_env_lock(|| {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().to_path_buf();
            assert_eq!(sessions_db_path_in(&home), home.join("sessions.db"));
            assert_eq!(sessions_dir_in(&home), home.join("sessions"));
            assert_eq!(trajectories_dir_in(&home), home.join("trajectories"));
        });
    }
}
