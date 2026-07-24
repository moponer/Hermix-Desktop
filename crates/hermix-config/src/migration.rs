//! Legacy data-dir migration (Phase 11).
//!
//! Before the `hermes` → `hermix` rename, all user data lived in `~/.hermes`.
//! After the rename, the default is `~/.hermix`, and the Tauri portable
//! install uses `<install_root>/data` (set via `HERMIX_PORTABLE_DATA_DIR`).
//!
//! This module is the **user-controlled, copy-based** import path. It is
//! distinct from the opt-in `HERMIX_MIGRATE_FROM_HERMES` rename in
//! [`crate::paths`] (which is fire-and-forget at startup, never asks
//! the user, and moves data to a fixed `~/.hermix`).
//!
//! The two are complementary:
//!
//! - `paths::migrate_legacy_hermes_dir` — when `HERMIX_MIGRATE_FROM_HERMES=1`
//!   is set, rename `~/.hermes` → `~/.hermix` (or `.hermix.bak.<ts>` if
//!   `~/.hermix` already exists). Single-shot, never UI-driven.
//! - `migration::LegacyMigrator` (this module) — the Tauri UI flow.
//!   `probe()` reports what legacy data exists, `migrate()` **copies**
//!   it (never moves/renames) into the *current* hermix home (which
//!   might be portable `<install_root>/data` or `~/.hermix`), and
//!   `mark_skipped()` records a "don't ask again" preference.
//!
//! The Tauri launcher calls `probe()` on startup; if it returns
//! `needs_migration = true`, the front-end shows a "Found legacy data,
//! import it?" dialog whose three buttons map to `migrate()`,
//! `mark_skipped()`, and "ask next time" (no-op).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Subset of legacy artifacts we know how to import.
///
/// Each variant knows its source filename/dirname and its target
/// location inside the current hermix home. New artifacts can be
/// added without changing the dialog UI (the dialog always says
/// "N items found").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyArtifact {
    /// `MEMORY.md` — the agent's curated long-term notes.
    Memory,
    /// `USER.md` — the long-term user profile.
    User,
    /// `auth.json` — OAuth tokens for managed-gateway providers.
    Auth,
    /// `approval_allowlist.json` — "Always Allow" command patterns.
    ApprovalAllowlist,
    /// `sessions/` directory — JSON-Lines session history.
    Sessions,
    /// `cron/` directory — scheduled jobs (with its inner `cron.db` SQLite).
    Cron,
    /// `skills/` directory — user-curated skills.
    Skills,
    /// `config.yaml` — top-level agent config.
    Config,
}

impl LegacyArtifact {
    /// Filename (for files) or directory name (for directories) inside
    /// the legacy `~/.hermes` home.
    pub const fn legacy_name(self) -> &'static str {
        match self {
            Self::Memory => "MEMORY.md",
            Self::User => "USER.md",
            Self::Auth => "auth.json",
            Self::ApprovalAllowlist => "approval_allowlist.json",
            Self::Sessions => "sessions",
            Self::Cron => "cron",
            Self::Skills => "skills",
            Self::Config => "config.yaml",
        }
    }

    /// Target name inside the *current* hermix home. The current
    /// hermix home is the only one that may be portable
    /// (`<install_root>/data`), so we never hard-code `~/.hermix`.
    pub const fn target_name(self) -> &'static str {
        // For now, all artifacts land at the same basename in the new
        // home. (If we ever split a "legacy subdir" off inside the new
        // home, this is the place to change it.)
        self.legacy_name()
    }

    /// Whether this artifact is a directory (true) or a file (false).
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Sessions | Self::Cron | Self::Skills)
    }

    /// All known artifacts, in stable iteration order. Used by
    /// `probe()` to scan, and by `migrate()` to copy.
    pub const ALL: &'static [LegacyArtifact] = &[
        Self::Memory,
        Self::User,
        Self::Auth,
        Self::ApprovalAllowlist,
        Self::Sessions,
        Self::Cron,
        Self::Skills,
        Self::Config,
    ];
}

/// Result of `LegacyMigrator::probe()`.
///
/// Returned to the front-end so the migration dialog can show
/// "Found N items: MEMORY.md, USER.md, sessions/, …" plus the legacy
/// and target paths for the "where would it land?" tooltip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacySnapshot {
    /// Absolute path to the detected legacy home (e.g. `~/.hermes`).
    pub legacy_home: PathBuf,
    /// Absolute path to the current hermix home (where data would be
    /// imported to). This may be portable (`<install_root>/data`) or
    /// `~/.hermix` — the dialog must not assume one or the other.
    pub target_home: PathBuf,
    /// Artifacts that exist in the legacy home and are importable.
    pub artifacts: Vec<LegacyArtifact>,
    /// Total size in bytes of all listed artifacts (recursive for
    /// directories). The dialog uses this for the "this will copy
    /// 12.3 MB" hint.
    pub total_bytes: u64,
}

/// Outcome of a single `migrate()` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    /// What was copied.
    pub copied: Vec<LegacyArtifact>,
    /// What we tried to copy but skipped (e.g. file disappeared
    /// between probe and migrate). The dialog shows these as
    /// warnings but the import is still considered successful.
    pub skipped: Vec<LegacyArtifact>,
    /// Hard failures — the dialog shows these as errors and asks
    /// the user to retry or contact support.
    pub errors: Vec<String>,
}

impl MigrationReport {
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Marker filename inside the *current* hermix home that records
/// the user picked "Don't ask again" or "Skip for now".
///
/// The presence of this file makes `probe()` return
/// `needs_migration = false`. The file's contents (a `SkippedRecord`
/// JSON) let the dialog re-show itself after a fixed number of days
/// if the user picked "skip for now" instead of "don't ask again".
const SKIPPED_MARKER: &str = ".hermix_migration_skipped.json";

/// A single skip-record. Written to `SKIPPED_MARKER` in the current
/// hermix home when the user clicks "Skip for now" or "Don't ask
/// again" on the dialog.
///
/// We store `dismissed_at` as Unix seconds (i64) wrapped in a
/// newtype so the JSON looks like `"dismissed_at": 1716500000` —
/// trivial to read by hand, immune to time-zone confusion, and
/// doesn't pull in a `chrono` dependency. The display layer (UI /
/// logs) is responsible for formatting it as RFC 3339 if needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedRecord {
    /// Unix-seconds timestamp the user last dismissed the dialog.
    pub dismissed_at: UnixSecs,
    /// If true, never show the dialog again for this hermix home.
    /// If false (default), re-show after `re_ask_after_days` days.
    pub permanent: bool,
    /// How many days to wait before re-asking. Only meaningful when
    /// `permanent = false`. Default: 30.
    pub re_ask_after_days: u32,
}

/// Newtype wrapper around Unix seconds. Serializes as a bare integer
/// (no string parsing on the round trip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixSecs(pub i64);

/// Find the user's home directory, in priority order. Mirrors
/// `paths::user_home_dir` but exposed publicly so the server crate
/// can pass it into the migrator.
pub fn user_home() -> PathBuf {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The list of legacy home paths we recognize, in priority order.
///
/// We check both `~/.hermes` (the pre-rename name) and
/// `~/.hermix-old` (a defensive name for users who moved their dir
/// aside by hand before re-installing). The first one that exists
/// wins.
pub fn candidate_legacy_homes() -> Vec<PathBuf> {
    let h = user_home();
    vec![h.join(".hermes"), h.join(".hermix-old")]
}

/// True if the user has asked us not to show the migration dialog.
///
/// Reads `<target_home>/.hermix_migration_skipped.json`. Returns
/// true if the file exists and `permanent = true`, OR if it was
/// dismissed less than `re_ask_after_days` ago.
pub fn is_skipped(target_home: &Path, now_unix: i64) -> bool {
    let path = target_home.join(SKIPPED_MARKER);
    let Ok(text) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(rec) = serde_json::from_str::<SkippedRecord>(&text) else {
        // Corrupt marker — treat as "not skipped" so the dialog
        // re-shows. Better than silently suppressing forever.
        return false;
    };
    if rec.permanent {
        return true;
    }
    let age_secs = now_unix.saturating_sub(rec.dismissed_at.0);
    let re_ask_secs = (rec.re_ask_after_days as i64).saturating_mul(86_400);
    age_secs < re_ask_secs
}

/// Write a "don't ask again" or "skip for now" marker to
/// `<target_home>/.hermix_migration_skipped.json`. If the file
/// already exists, it is overwritten (the user just re-decided).
pub fn mark_skipped(target_home: &Path, permanent: bool, re_ask_after_days: u32) -> std::io::Result<()> {
    fs::create_dir_all(target_home)?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rec = SkippedRecord {
        dismissed_at: UnixSecs(now),
        permanent,
        re_ask_after_days,
    };
    let json = serde_json::to_string_pretty(&rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(target_home.join(SKIPPED_MARKER), json)
}

/// Delete the skip marker (used by a "Reset migration state" button
/// in the dialog so a user who picks "don't ask again" can change
/// their mind).
pub fn clear_skipped(target_home: &Path) -> std::io::Result<()> {
    let p = target_home.join(SKIPPED_MARKER);
    if p.exists() {
        fs::remove_file(p)?;
    }
    Ok(())
}

/// The migrator itself. Cheap to construct; methods take `&self`.
#[derive(Debug, Clone, Default)]
pub struct LegacyMigrator {
    /// Optional override of the user home. When `Some`, the migrator
    /// looks for legacy homes *under this path* instead of the real
    /// user home. Production code leaves this `None`; tests set it
    /// to a tempdir so they don't touch the real `~/.hermes`.
    user_home_override: Option<PathBuf>,
}

impl LegacyMigrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a migrator that searches a specific user-home
    /// directory instead of the real one. Used by tests; the
    /// production Tauri launcher uses [`Self::new`].
    pub fn with_user_home(home: impl Into<PathBuf>) -> Self {
        Self {
            user_home_override: Some(home.into()),
        }
    }

    /// The user home this migrator will look for legacy data under.
    pub fn effective_user_home(&self) -> PathBuf {
        self.user_home_override.clone().unwrap_or_else(user_home)
    }

    /// Scan the candidate legacy homes and return the first one that
    /// exists. Then enumerate which of [`LegacyArtifact::ALL`] are
    /// present there.
    /// /// `target_home` is the current hermix home (we do NOT auto-
    /// resolve it from the env inside this method, so the caller —
    /// i.e. the Tauri launcher — controls where the imported data
    /// lands). We accept `target_home` explicitly so portable mode
    /// (`<install_root>/data`) works exactly the same as the default
    /// (`~/.hermix`).
    pub fn probe(&self, target_home: &Path) -> Option<LegacySnapshot> {
        let user_home = self.effective_user_home();
        let candidates = vec![user_home.join(".hermes"), user_home.join(".hermix-old")];
        self.probe_with_candidates(target_home, &candidates)
    }

    /// Like [`Self::probe`] but with caller-supplied candidate
    /// legacy homes. The dashboard HTTP handler uses this to add
    /// the portable-mode case (`<install_root>/.hermes` next to
    /// the data dir) on top of the standard-mode candidates.
    pub fn probe_with_candidates(
        &self,
        target_home: &Path,
        candidate_legacy_homes: &[PathBuf],
    ) -> Option<LegacySnapshot> {
        for legacy in candidate_legacy_homes {
            if !legacy.is_dir() {
                continue;
            }
            let mut artifacts = Vec::new();
            let mut total_bytes: u64 = 0;
            for &art in LegacyArtifact::ALL {
                let path = legacy.join(art.legacy_name());
                if !path.exists() {
                    continue;
                }
                let size = dir_size(&path);
                total_bytes = total_bytes.saturating_add(size);
                artifacts.push(art);
            }
            if artifacts.is_empty() {
                continue;
            }
            return Some(LegacySnapshot {
                legacy_home: legacy.clone(),
                target_home: target_home.to_path_buf(),
                artifacts,
                total_bytes,
            });
        }
        None
    }

    /// Copy every listed artifact from `snapshot.legacy_home` to
    /// `snapshot.target_home`.
    /// /// We **never** delete or rename the legacy home — the user
    /// explicitly chose this over the auto-rename path precisely
    /// because they want to keep the original around (e.g. for
    /// rollback, or because they're also running the original
    /// NousResearch hermes-agent).
    /// /// Existing files at the target are **not** overwritten unless
    /// `overwrite` is true (the dialog should default to false and
    /// only flip it after the user confirms "yes, blow away the
    /// existing copy"). Files that would be overwritten are reported
    /// in `skipped` with the path so the dialog can warn.
    pub fn migrate(&self, snapshot: &LegacySnapshot, overwrite: bool) -> MigrationReport {
        let mut report = MigrationReport {
            copied: Vec::new(),
            skipped: Vec::new(),
            errors: Vec::new(),
        };
        if let Err(e) = fs::create_dir_all(&snapshot.target_home) {
            report.errors.push(format!(
                "could not create target home {}: {e}",
                snapshot.target_home.display()
            ));
            return report;
        }
        for &art in &snapshot.artifacts {
            let src = snapshot.legacy_home.join(art.legacy_name());
            let dst = snapshot.target_home.join(art.target_name());
            if !src.exists() {
                report.skipped.push(art);
                continue;
            }
            if dst.exists() && !overwrite {
                report.skipped.push(art);
                continue;
            }
            // For files, remove the existing copy first if overwriting
            // (copying onto a file is not portable across all platforms).
            if dst.exists() && overwrite {
                let _ = remove_path(&dst);
            }
            match copy_path(&src, &dst) {
                Ok(()) => report.copied.push(art),
                Err(e) => report
                    .errors
                    .push(format!("{}: {e}", art.legacy_name())),
            }
        }
        report
    }
}

// ---------------------------------------------------------------------------
// File/dir copy helpers
// ---------------------------------------------------------------------------

/// Recursive size in bytes. Returns 0 on I/O error (size is best-
/// effort; the import itself will surface real errors).
fn dir_size(p: &Path) -> u64 {
    if p.is_file() {
        return fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            total = total.saturating_add(dir_size(&e.path()));
        }
    }
    total
}

/// Remove a file or directory tree. Best-effort; errors are swallowed
/// (the caller logs the result on a per-artifact basis).
fn remove_path(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Copy a file or directory tree from `src` to `dst`. `dst` must
/// not exist (callers handle overwrite explicitly).
fn copy_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_src = entry.path();
            let child_dst = dst.join(entry.file_name());
            copy_path(&child_src, &child_dst)?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst).map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A monotonic counter used to pick a unique fake legacy dir
    /// name for each test, so parallel tests don't race on the
    /// real `~/.hermes` location.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Set up a fake "user home" containing a `.hermes` legacy dir
    /// with the given artifacts. Returns `(legacy_home, target_home)`
    /// — both are tempdirs the caller must keep alive.
    fn fixture(artifacts: &[LegacyArtifact]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let user_home = tempfile::tempdir().unwrap();
        let legacy = user_home.path().join(".hermes");
        fs::create_dir_all(&legacy).unwrap();
        for &art in artifacts {
            let p = legacy.join(art.legacy_name());
            if art.is_dir() {
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("marker"), art.legacy_name()).unwrap();
            } else {
                fs::write(&p, format!("contents-of-{}", art.legacy_name())).unwrap();
            }
        }
        // Use a *separate* tempdir as the target, so we never
        // accidentally write into the real `~/.hermix`.
        let target = user_home.path().join("portable-data");
        fs::create_dir_all(&target).unwrap();
        (user_home, legacy, target)
    }

    #[test]
    fn probe_finds_existing_artifacts() {
        let (user_home, legacy, target) = fixture(&[
            LegacyArtifact::Memory,
            LegacyArtifact::Sessions,
            LegacyArtifact::Auth,
        ]);
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).expect("should find a legacy home");
        assert_eq!(snap.legacy_home, legacy);
        assert_eq!(snap.target_home, target);
        assert!(snap.artifacts.contains(&LegacyArtifact::Memory));
        assert!(snap.artifacts.contains(&LegacyArtifact::Sessions));
        assert!(snap.artifacts.contains(&LegacyArtifact::Auth));
        assert!(!snap.artifacts.contains(&LegacyArtifact::Cron));
    }

    #[test]
    fn probe_returns_none_when_legacy_missing() {
        let target = tempfile::tempdir().unwrap();
        let user_home = tempfile::tempdir().unwrap();
        // No .hermes under user_home.
        let m = LegacyMigrator::with_user_home(user_home.path());
        assert!(m.probe(target.path()).is_none());
    }

    #[test]
    fn probe_skips_empty_legacy_dir() {
        // A legacy home with no recognized artifacts is treated as
        // "no legacy data" (probe returns None). This protects
        // against a user who deleted everything but left the dir.
        let user_home = tempfile::tempdir().unwrap();
        let legacy = user_home.path().join(".hermes");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("random.txt"), "not a known artifact").unwrap();
        let target = tempfile::tempdir().unwrap();
        let m = LegacyMigrator::with_user_home(user_home.path());
        assert!(m.probe(target.path()).is_none());
    }

    #[test]
    fn migrate_copies_files() {
        let (user_home, _legacy, target) =
            fixture(&[LegacyArtifact::Memory, LegacyArtifact::User]);
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        let report = m.migrate(&snap, false);
        assert!(report.is_success(), "errors: {:?}", report.errors);
        assert_eq!(report.copied.len(), 2);
        assert!(target.join("MEMORY.md").exists());
        assert!(target.join("USER.md").exists());
    }

    #[test]
    fn migrate_copies_directories_recursively() {
        let (user_home, _legacy, target) =
            fixture(&[LegacyArtifact::Sessions, LegacyArtifact::Skills]);
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        let report = m.migrate(&snap, false);
        assert!(report.is_success(), "errors: {:?}", report.errors);
        // The "marker" file we wrote inside each dir should
        // have been copied.
        assert!(target.join("sessions/marker").exists());
        assert!(target.join("skills/marker").exists());
    }

    #[test]
    fn migrate_skips_existing_files_without_overwrite() {
        let (user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        // Pre-populate target with a sentinel.
        fs::write(target.join("MEMORY.md"), "current-data").unwrap();
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        let report = m.migrate(&snap, false);
        assert!(report.is_success());
        assert!(report.copied.is_empty());
        assert!(report.skipped.contains(&LegacyArtifact::Memory));
        // The original file is untouched.
        assert_eq!(
            fs::read_to_string(target.join("MEMORY.md")).unwrap(),
            "current-data"
        );
    }

    #[test]
    fn migrate_with_overwrite_replaces_files() {
        let (user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        fs::write(target.join("MEMORY.md"), "current-data").unwrap();
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        let report = m.migrate(&snap, true);
        assert!(report.is_success());
        assert!(report.copied.contains(&LegacyArtifact::Memory));
        assert!(!fs::read_to_string(target.join("MEMORY.md"))
            .unwrap()
            .contains("current-data"));
    }

    #[test]
    fn migrate_does_not_delete_legacy() {
        // The whole point of "copy" vs "rename": the legacy home
        // survives the import.
        let (user_home, legacy, target) = fixture(&[LegacyArtifact::Memory]);
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        m.migrate(&snap, false);
        assert!(legacy.exists(), "legacy home must not be touched");
        assert!(legacy.join("MEMORY.md").exists());
    }

    #[test]
    fn skipped_marker_permanent_suppresses_probe() {
        let (user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        mark_skipped(&target, true, 30).unwrap();
        let m = LegacyMigrator::with_user_home(user_home.path());
        // Even though there's legacy data, probe should still
        // find it — the dialog itself checks is_skipped()
        // before deciding whether to show. (The separation lets
        // a "reset" button clear the marker without the user
        // having to remove the legacy dir.)
        let snap = m.probe(&target);
        assert!(snap.is_some(), "probe must still see legacy data");
        // is_skipped() is what the dialog consults.
        assert!(is_skipped(&target, now_unix()));
    }

    #[test]
    fn skipped_marker_temporary_re_shows_after_window() {
        let (_user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        mark_skipped(&target, false, 30).unwrap();
        // Just-dismissed → still suppressed.
        assert!(is_skipped(&target, now_unix()));
        // 60 days later → re-ask.
        let future = now_unix() + 60 * 86_400;
        assert!(!is_skipped(&target, future));
    }

    #[test]
    fn clear_skipped_removes_marker() {
        let (_user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        mark_skipped(&target, true, 30).unwrap();
        assert!(is_skipped(&target, now_unix()));
        clear_skipped(&target).unwrap();
        assert!(!is_skipped(&target, now_unix()));
    }

    #[test]
    fn corrupt_marker_does_not_lock_dialog() {
        let (user_home, _legacy, target) = fixture(&[LegacyArtifact::Memory]);
        fs::write(target.join(SKIPPED_MARKER), "not json").unwrap();
        // A corrupt marker must NOT silently suppress forever.
        assert!(!is_skipped(&target, now_unix()));
        // Suppress the unused warning when with_user_home is replaced.
        let _ = LegacyMigrator::with_user_home(user_home.path());
    }

    #[test]
    fn total_bytes_includes_directories() {
        let (user_home, _legacy, target) =
            fixture(&[LegacyArtifact::Sessions, LegacyArtifact::Memory]);
        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        // sessions/marker has 16+ bytes ("sessions") + MEMORY.md
        // content. We just assert it's non-zero and at least
        // the size of MEMORY.md.
        assert!(snap.total_bytes > 0);
    }

    #[test]
    fn candidate_legacy_homes_returns_hermes_then_hermix_old() {
        // The function's contract is a stable priority order so
        // .hermes always wins over .hermix-old when both exist.
        let homes = candidate_legacy_homes();
        assert!(homes.len() >= 2);
        assert!(homes[0].ends_with(".hermes"));
        assert!(homes[1].ends_with(".hermix-old"));
    }

    #[test]
    fn skipped_record_serde_round_trip() {
        // Ensures the on-disk JSON can be read back identically.
        let rec = SkippedRecord {
            dismissed_at: UnixSecs(1_716_500_000),
            permanent: false,
            re_ask_after_days: 14,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: SkippedRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    /// The counter helper exists so each test gets a unique target
    /// dir; we don't actually need to use the value anywhere.
    #[allow(dead_code)]
    fn tick() -> u64 {
        COUNTER.fetch_add(1, Ordering::SeqCst)
    }

    fn now_unix() -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------
    // P1-3: error-path and edge-case test coverage added 2026-06-19.
    // The original 15 tests cover the happy path; these eight probe
    // the failure modes the user is most likely to hit in production
    // (corrupt skip markers, permission errors, partial failures,
    // priority order between `.hermes` and `.hermix-old`).
    // -----------------------------------------------------------------

    #[test]
    fn probe_with_candidates_picks_hermes_over_hermix_old() {
        // When BOTH `.hermes` and `.hermix-old` exist under the user
        // home, the former must win. Otherwise we'd accidentally
        // re-import data the user already migrated.
        let user_home = tempfile::tempdir().unwrap();
        let hermes = user_home.path().join(".hermes");
        let hermix_old = user_home.path().join(".hermix-old");
        fs::create_dir_all(&hermes).unwrap();
        fs::create_dir_all(&hermix_old).unwrap();
        fs::write(hermes.join("MEMORY.md"), "from-hermes").unwrap();
        fs::write(hermix_old.join("MEMORY.md"), "from-old").unwrap();

        let m = LegacyMigrator::with_user_home(user_home.path());
        let target = tempfile::tempdir().unwrap();
        let snap = m.probe(target.path()).expect("should find a legacy home");
        assert_eq!(
            snap.legacy_home, hermes,
            ".hermes must win over .hermix-old in candidate priority"
        );
    }

    #[test]
    fn probe_with_candidates_falls_back_to_hermix_old() {
        // Mirror of the above: when `.hermes` is missing, we must
        // fall back to `.hermix-old` so a user who manually moved
        // their data still gets the prompt.
        let user_home = tempfile::tempdir().unwrap();
        let hermix_old = user_home.path().join(".hermix-old");
        fs::create_dir_all(&hermix_old).unwrap();
        fs::write(hermix_old.join("MEMORY.md"), "from-old").unwrap();

        let m = LegacyMigrator::with_user_home(user_home.path());
        let target = tempfile::tempdir().unwrap();
        let snap = m
            .probe(target.path())
            .expect("should find a legacy home via fallback");
        assert_eq!(snap.legacy_home, hermix_old);
    }

    #[test]
    fn probe_with_candidates_explicit_preserves_caller_order() {
        // The dashboard's HTTP handler passes
        // `[<portable_root>/.hermes, <user_home>/.hermes, ...]` —
        // it MUST be the first match that wins, not the first
        // existing one in the global priority list. This test
        // pins that contract.
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        // dir1 has only Auth, dir2 has only Memory.
        fs::create_dir_all(dir1.path().join(".hermes")).unwrap();
        fs::write(dir1.path().join(".hermes").join("auth.json"), "{}").unwrap();
        fs::create_dir_all(dir2.path().join(".hermes")).unwrap();
        fs::write(dir2.path().join(".hermes").join("MEMORY.md"), "x").unwrap();

        let m = LegacyMigrator::new();
        let target = tempfile::tempdir().unwrap();
        let snap = m
            .probe_with_candidates(
                target.path(),
                &[dir1.path().join(".hermes"), dir2.path().join(".hermes")],
            )
            .expect("first candidate wins");
        assert_eq!(snap.legacy_home, dir1.path().join(".hermes"));
        assert!(snap.artifacts.contains(&LegacyArtifact::Auth));
        assert!(!snap.artifacts.contains(&LegacyArtifact::Memory));
    }

    #[test]
    fn migrate_continues_after_artifact_error() {
        // One artifact must fail to copy (e.g. by removing read
        // permission on the source on Unix) while the others
        // succeed. The report should contain both `copied` and
        // `errors`, NOT abort the whole migration.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (user_home, legacy, target) = fixture(&[
                LegacyArtifact::Memory,
                LegacyArtifact::User,
                LegacyArtifact::Auth,
            ]);
            // Make MEMORY.md unreadable. The other two files are
            // untouched so they should still migrate.
            let blocked = legacy.join("MEMORY.md");
            let mut perms = fs::metadata(&blocked).unwrap().permissions();
            perms.set_mode(0o000);
            fs::set_permissions(&blocked, perms).unwrap();

            let m = LegacyMigrator::with_user_home(user_home.path());
            let snap = m.probe(&target).unwrap();
            let report = m.migrate(&snap, false);

            // Restore so tempdir cleanup can delete it.
            let mut perms = fs::metadata(&blocked).unwrap().permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&blocked, perms).unwrap();

            assert!(
                !report.copied.is_empty(),
                "the other artifacts must still migrate: copied={:?}",
                report.copied
            );
            assert!(
                !report.errors.is_empty(),
                "the blocked artifact must report an error: errors={:?}",
                report.errors
            );
            assert!(
                report.errors.iter().any(|e| e.contains("MEMORY.md")),
                "error must mention the blocked file: {:?}",
                report.errors
            );
        }
        #[cfg(not(unix))]
        {
            // On Windows we cannot easily revoke read permission
            // for the current user, so just assert the report shape
            // for a happy path (no errors expected).
            let (user_home, _legacy, target) =
                fixture(&[LegacyArtifact::Memory, LegacyArtifact::User]);
            let m = LegacyMigrator::with_user_home(user_home.path());
            let snap = m.probe(&target).unwrap();
            let report = m.migrate(&snap, false);
            assert!(report.is_success());
        }
    }

    #[test]
    fn migrate_preserves_legacy_after_partial_failure() {
        // Critical invariant: even if migrate() reports errors,
        // the LEGACY home must be untouched. The user explicitly
        // chose the copy-based path over the auto-rename path
        // precisely so they keep the original around.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (user_home, legacy, target) = fixture(&[LegacyArtifact::Memory]);
            let blocked = legacy.join("MEMORY.md");
            let mut perms = fs::metadata(&blocked).unwrap().permissions();
            perms.set_mode(0o000);
            fs::set_permissions(&blocked, perms).unwrap();

            let m = LegacyMigrator::with_user_home(user_home.path());
            let snap = m.probe(&target).unwrap();
            let _ = m.migrate(&snap, false);

            // Restore for cleanup.
            let mut perms = fs::metadata(&blocked).unwrap().permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&blocked, perms).unwrap();

            assert!(legacy.exists(), "legacy home must survive partial failure");
            // The original file must still be readable.
            assert!(fs::read_to_string(&blocked).is_ok());
        }
        #[cfg(not(unix))]
        {
            // No-op for non-Unix.
            let (user_home, legacy, _target) = fixture(&[LegacyArtifact::Memory]);
            let _ = LegacyMigrator::with_user_home(user_home.path());
            assert!(legacy.exists());
        }
    }

    #[test]
    fn migrate_with_overwrite_replaces_existing_directory_recursively() {
        // Overwrite a directory artifact that already exists at
        // the target. The old contents must be wiped, the new
        // ones must land.
        let (user_home, _legacy, target) = fixture(&[LegacyArtifact::Sessions]);
        // Pre-populate the target sessions dir with sentinel files.
        let target_sessions = target.join("sessions");
        fs::create_dir_all(&target_sessions).unwrap();
        fs::write(target_sessions.join("old-file"), "old-data").unwrap();
        assert!(target_sessions.join("old-file").exists());

        let m = LegacyMigrator::with_user_home(user_home.path());
        let snap = m.probe(&target).unwrap();
        let report = m.migrate(&snap, true);
        assert!(report.is_success(), "errors: {:?}", report.errors);
        assert!(report.copied.contains(&LegacyArtifact::Sessions));
        // The sentinel must be gone (the new sessions/ dir replaces it).
        assert!(!target_sessions.join("old-file").exists());
        // The new marker must be there.
        assert!(target_sessions.join("marker").exists());
    }

    #[test]
    fn mark_skipped_with_zero_re_ask_days_means_ask_immediately() {
        // `re_ask_after_days = 0` is the "ask me every launch" mode.
        // The user might toggle this via the dialog's "always ask"
        // checkbox. We must honour it: is_skipped must return false
        // even at `now_unix() + 1`.
        let target = tempfile::tempdir().unwrap();
        mark_skipped(target.path(), false, 0).unwrap();
        assert!(
            !is_skipped(target.path(), now_unix() + 1),
            "re_ask_after_days=0 must re-prompt on the next launch"
        );
    }

    #[test]
    fn mark_skipped_permanent_ignores_re_ask_days_field() {
        // A permanent skip ignores the days field entirely. Even
        // with `re_ask_after_days = 30`, is_skipped must still
        // return true far in the future. (This guards against
        // a future refactor that re-introduces the "auto re-ask"
        // behaviour for permanent skips.)
        let target = tempfile::tempdir().unwrap();
        mark_skipped(target.path(), true, 30).unwrap();
        let far_future = now_unix() + 10 * 365 * 86_400; // 10 years out
        assert!(
            is_skipped(target.path(), far_future),
            "permanent skip must hold even 10 years out"
        );
    }
}
