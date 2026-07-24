//! Migration dashboard endpoints (Phase 11).
//!
//! Three endpoints back the Tauri migration dialog:
//!
//! - `GET  /api/migration/probe`   → `MigrationStatusResponse`
//! - `POST /api/migration/apply`   → `MigrationReport`
//! - `POST /api/migration/skip`    → `SkipResponse`
//!
//! All three share `HttpServerState::hermix_home` (the *current*,
//! resolved hermix home) as the import target. The probe endpoint
//! also consults the skip-marker under that same target so the
//! dialog can decide whether to even show itself.
//!
//! The endpoints are intentionally thin: they delegate the real
//! work to [`hermix_config::migration`]. This keeps the
//! configuration-crate testable without an axum server, and keeps
//! the server crate a pure HTTP adapter.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use hermix_config::migration::{
    is_skipped, mark_skipped, clear_skipped, user_home as system_user_home,
    LegacyMigrator, LegacySnapshot, MigrationReport,
};

use crate::HttpServerState;

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// Returned by `GET /api/migration/probe`. The front-end uses
/// `should_show_dialog` directly to decide whether to render the
/// dialog component; the rest of the fields feed the dialog body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatusResponse {
    /// True iff the dialog should be visible. False when:
    /// - no legacy data is found
    /// - the user previously picked "don't ask again"
    /// - the user previously picked "skip for now" less than
    /// `re_ask_after_days` ago
    pub should_show_dialog: bool,
    /// Why the dialog is or isn't showing. Useful for the "Why am I
    /// seeing this?" hover tooltip.
    pub reason: String,
    /// The detected legacy snapshot (legacy home + artifacts + size).
    /// `None` when there's no legacy data to import.
    pub snapshot: Option<LegacySnapshot>,
    /// When the user last dismissed the dialog, if ever. `None` if
    /// they've never been asked.
    pub skipped_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ApplyRequest {
    /// Whether to overwrite files that already exist in the target.
    /// Defaults to `false` server-side; the front-end should ask
    /// the user before flipping this to `true`.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Deserialize)]
pub struct SkipRequest {
    /// `true` ⇒ "don't ask again" (permanent). `false` (default)
    /// ⇒ "skip for now" (re-ask after `re_ask_after_days`).
    #[serde(default)]
    pub permanent: bool,
    /// How many days before re-asking. Defaults to 30.
    #[serde(default = "default_re_ask_days")]
    pub re_ask_after_days: u32,
}

fn default_re_ask_days() -> u32 {
    30
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SkipResponse {
    pub recorded: bool,
    pub permanent: bool,
    pub re_ask_after_days: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResetResponse {
    pub cleared: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/migration/probe`
///
/// Single round-trip for the dialog: gives the front-end everything
/// it needs to decide whether to show, and if so, what to display.
pub async fn probe_migration(
    State(state): State<HttpServerState>,
) -> Json<MigrationStatusResponse> {
    let migrator = LegacyMigrator::new();
    let target = state.hermix_home.clone();
    let candidates = legacy_candidates(&target);
    let snapshot = migrator.probe_with_candidates(&target, &candidates);

    // Read the skip-marker directly (rather than going through the
    // `is_skipped` helper with a clock) so the response can also
    // report the raw timestamp.
    let skipped_at = read_skipped_at(&target);

    if snapshot.is_none() {
        return Json(MigrationStatusResponse {
            should_show_dialog: false,
            reason: format!(
                "no legacy data found in the standard locations ({})",
                human_candidates(&candidates)
            ),
            snapshot: None,
            skipped_at,
        });
    }

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if is_skipped(&target, now_unix) {
        return Json(MigrationStatusResponse {
            should_show_dialog: false,
            reason: "user previously dismissed the dialog".to_string(),
            snapshot,
            skipped_at,
        });
    }

    Json(MigrationStatusResponse {
        should_show_dialog: true,
        reason: format!(
            "found {} legacy item(s) (~{} bytes)",
            snapshot.as_ref().map(|s| s.artifacts.len()).unwrap_or(0),
            snapshot.as_ref().map(|s| s.total_bytes).unwrap_or(0)
        ),
        snapshot,
        skipped_at,
    })
}

/// `POST /api/migration/apply`
///
/// Runs the copy. Returns a [`MigrationReport`] with per-artifact
/// status. Errors at the I/O level (e.g. the legacy home
/// disappeared between probe and apply) are mapped to HTTP 500;
/// the report itself is always present in the body so the dialog
/// can render a partial-success view.
pub async fn apply_migration(
    State(state): State<HttpServerState>,
    Json(req): Json<ApplyRequest>,
) -> Response {
    let migrator = LegacyMigrator::new();
    let target = state.hermix_home.clone();
    let candidates = legacy_candidates(&target);
    let Some(snapshot) = migrator.probe_with_candidates(&target, &candidates) else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "no legacy data found; nothing to import"
            })),
        )
            .into_response();
    };
    let report: MigrationReport = migrator.migrate(&snapshot, req.overwrite);
    let status = if report.is_success() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(report)).into_response()
}

/// `POST /api/migration/skip`
///
/// Records the user's "don't ask" / "skip for now" decision. The
/// response echoes what was written so the front-end can show a
/// confirmation ("We'll ask again in 30 days" vs "Got it, won't
/// ask again").
pub async fn skip_migration(
    State(state): State<HttpServerState>,
    Json(req): Json<SkipRequest>,
) -> Result<Json<SkipResponse>, MigrationApiError> {
    mark_skipped(&state.hermix_home, req.permanent, req.re_ask_after_days)
        .map_err(|e| MigrationApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("could not write skip marker: {e}"),
        })?;
    Ok(Json(SkipResponse {
        recorded: true,
        permanent: req.permanent,
        re_ask_after_days: req.re_ask_after_days,
    }))
}

/// `POST /api/migration/reset`
///
/// Removes the skip marker. The dialog can wire a "you can always
/// re-enable this from Settings → Migration" button to this.
pub async fn reset_migration(
    State(state): State<HttpServerState>,
) -> Result<Json<ResetResponse>, MigrationApiError> {
    clear_skipped(&state.hermix_home).map_err(|e| MigrationApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("could not clear skip marker: {e}"),
    })?;
    Ok(Json(ResetResponse { cleared: true }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the `dismissed_at` timestamp out of the skip-marker file, if
/// one is present and parseable. Used by the probe endpoint to tell
/// the UI "you were asked on X" without re-deriving it.
fn read_skipped_at(target_home: &std::path::Path) -> Option<i64> {
    let path = target_home.join(".hermix_migration_skipped.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let rec: hermix_config::migration::SkippedRecord = serde_json::from_str(&text).ok()?;
    Some(rec.dismissed_at.0)
}

/// Build the list of legacy-home candidates the dashboard hands to
/// the migrator. Order matters — the first existing directory wins,
/// so we put the portable-mode path (next to the *current* hermix
/// home) first, then the standard-mode paths in the real user home.
///
/// Rationale: in portable mode `<install_root>/data` is the
/// hermix home, but a user upgrading from a standard install on
/// the same machine may still have `~/.hermes` from a prior
/// portable install that used to be unpacked into a different
/// location. Checking the parent of the data dir first catches
/// that case without polluting the user home.
///
/// Test escape hatch: when the env var
/// `HERMIX_MIGRATION_PROBE_CANDIDATES` is set to a `|`-separated
/// list of paths, those paths are used as the candidate list and
/// the default computation is skipped entirely. Production never
/// sets this; tests set it to their own tempdir so they can run
/// in parallel without racing on the real `~/.hermes`. (We use
/// `|` rather than `:` because Windows paths themselves contain
/// `:` after the drive letter — `C:\Users\…`.)
fn legacy_candidates(target_home: &std::path::Path) -> Vec<std::path::PathBuf> {
    if let Ok(override_list) = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES") {
        if !override_list.is_empty() {
            return override_list
                .split('|')
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from)
                .collect();
        }
    }
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    if let Some(parent) = target_home.parent() {
        out.push(parent.join(".hermes"));
        out.push(parent.join(".hermix-old"));
    }
    let uh = system_user_home();
    out.push(uh.join(".hermes"));
    out.push(uh.join(".hermix-old"));
    out
}

fn human_candidates(cands: &[std::path::PathBuf]) -> String {
    cands
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// JSON error shape for endpoints that can fail outside the report
/// (e.g. I/O on the skip-marker). Kept separate from
/// `crate::HttpError` because the migration endpoints are added in
/// a phase that doesn't want to refactor the existing error type.
#[derive(Debug)]
pub struct MigrationApiError {
    pub status: StatusCode,
    pub message: String,
}

impl IntoResponse for MigrationApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state::test_state_with_temp_home;
    use hermix_config::migration::LegacyArtifact;

    /// Global mutex for tests that touch process-wide env vars.
    /// The migration probe reads `HERMIX_MIGRATION_PROBE_CANDIDATES`
    /// from `std::env::var`, which is process-wide. If two tests
    /// run in parallel and both set/restore that var, they can
    /// interleave and one test ends up reading the other's path.
    /// Locking this mutex around every env-mutating test makes
    /// them run serially — they are fast, so the cost is
    /// negligible, and it keeps each test's intent isolated.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// `/api/migration/probe` returns `should_show_dialog: false`
    /// when there's no legacy data.
    /// /// `HERMIX_MIGRATION_PROBE_CANDIDATES` is pinned to an empty
    /// `nonexistent_/.hermes` path so the probe ignores the
    /// developer's real `~/.hermes` (otherwise this test would
    /// spuriously "find" the developer's data and fail).
    #[tokio::test]
    async fn probe_returns_none_when_no_legacy() {
        let _g = env_lock();
        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                "definitely_nonexistent_path_/.hermes",
            );
        }
        let state = test_state_with_temp_home().await;
        let resp = probe_migration(State(state)).await;
        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert!(!resp.0.should_show_dialog);
        assert!(resp.0.snapshot.is_none());
    }

    /// `/api/migration/probe` returns the snapshot when a legacy
    /// home exists in the standard location.
    #[tokio::test]
    async fn probe_finds_legacy_home() {
        let _g = env_lock();
        let state = test_state_with_temp_home().await;
        // Create a legacy .hermes dir under the test user home with
        // a recognizable artifact. The temp home is the *target*,
        // so we drop a sibling .hermes next to it.
        let user_home = state
            .hermix_home
            .parent()
            .expect("temp home must have a parent")
            .to_path_buf();
        let legacy = user_home.join(".hermes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("MEMORY.md"), "old memory").unwrap();
        std::fs::write(legacy.join("USER.md"), "old user").unwrap();

        // Pin the candidate list to *our* tempdir so the
        // developer's real `~/.hermes` doesn't shadow it.
        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                legacy.to_string_lossy().as_ref(),
            );
        }
        let resp = probe_migration(State(state)).await;
        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert!(resp.0.should_show_dialog);
        let snap = resp.0.snapshot.expect("snapshot must be present");
        assert_eq!(snap.artifacts.len(), 2);
        assert!(snap.artifacts.contains(&LegacyArtifact::Memory));
        assert!(snap.artifacts.contains(&LegacyArtifact::User));
        assert!(snap.total_bytes > 0);

        // Cleanup so we don't leave a .hermes in the tempdir.
        let _ = std::fs::remove_dir_all(&legacy);
    }

    /// `/api/migration/apply` actually copies the files.
    #[tokio::test]
    async fn apply_copies_legacy_files() {
        let _g = env_lock();
        let state = test_state_with_temp_home().await;
        let user_home = state
            .hermix_home
            .parent()
            .expect("temp home must have a parent")
            .to_path_buf();
        let legacy = user_home.join(".hermes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("MEMORY.md"), "imported memory").unwrap();

        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                legacy.to_string_lossy().as_ref(),
            );
        }
        let resp = apply_migration(
            State(state.clone()),
            Json(ApplyRequest { overwrite: false }),
        )
        .await;
        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.hermix_home.join("MEMORY.md").exists());
        assert_eq!(
            std::fs::read_to_string(state.hermix_home.join("MEMORY.md")).unwrap(),
            "imported memory"
        );

        let _ = std::fs::remove_dir_all(&legacy);
    }

    /// `/api/migration/apply` returns 409 when there's no legacy
    /// data (rather than 200-with-empty-report, which would be
    /// ambiguous for the front-end).
    #[tokio::test]
    async fn apply_returns_409_when_nothing_to_import() {
        let _g = env_lock();
        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                "definitely_nonexistent_path_/.hermes",
            );
        }
        let state = test_state_with_temp_home().await;
        let resp = apply_migration(
            State(state),
            Json(ApplyRequest { overwrite: false }),
        )
        .await;
        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// `/api/migration/skip` writes the marker and `/probe` then
    /// reports `should_show_dialog: false`.
    #[tokio::test]
    async fn skip_then_probe_returns_should_not_show() {
        let _g = env_lock();
        let state = test_state_with_temp_home().await;
        let user_home = state
            .hermix_home
            .parent()
            .expect("temp home must have a parent")
            .to_path_buf();
        let legacy = user_home.join(".hermes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("MEMORY.md"), "old").unwrap();

        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                legacy.to_string_lossy().as_ref(),
            );
        }

        // First probe: dialog should show.
        let resp = probe_migration(State(state.clone())).await;
        assert!(resp.0.should_show_dialog);

        // Skip.
        let _ = skip_migration(
            State(state.clone()),
            Json(SkipRequest {
                permanent: true,
                re_ask_after_days: 30,
            }),
        )
        .await
        .expect("skip must succeed");

        // Probe again: dialog must NOT show.
        let resp = probe_migration(State(state)).await;

        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert!(!resp.0.should_show_dialog);
        assert!(resp.0.skipped_at.is_some());

        let _ = std::fs::remove_dir_all(&legacy);
    }

    /// `/api/migration/reset` removes the marker so the dialog
    /// shows again.
    #[tokio::test]
    async fn reset_clears_skip_marker() {
        let state = test_state_with_temp_home().await;
        let _ = skip_migration(
            State(state.clone()),
            Json(SkipRequest {
                permanent: true,
                re_ask_after_days: 30,
            }),
        )
        .await
        .expect("skip must succeed");

        let marker = state.hermix_home.join(".hermix_migration_skipped.json");
        assert!(marker.exists());

        let resp = reset_migration(State(state.clone())).await.expect("reset must succeed");
        assert!(resp.0.cleared);
        assert!(!marker.exists());

        let _ = std::fs::remove_dir_all(&marker);
    }

    /// Apply with `overwrite: true` replaces an existing file at the
    /// target. This is the "yes, blow it away" path the dialog
    /// exposes after warning the user.
    #[tokio::test]
    async fn apply_with_overwrite_replaces_existing() {
        let _g = env_lock();
        let state = test_state_with_temp_home().await;
        let user_home = state
            .hermix_home
            .parent()
            .expect("temp home must have a parent")
            .to_path_buf();
        let legacy = user_home.join(".hermes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("MEMORY.md"), "imported").unwrap();
        std::fs::write(state.hermix_home.join("MEMORY.md"), "current").unwrap();

        let saved = std::env::var("HERMIX_MIGRATION_PROBE_CANDIDATES").ok();
        unsafe {
            std::env::set_var(
                "HERMIX_MIGRATION_PROBE_CANDIDATES",
                legacy.to_string_lossy().as_ref(),
            );
        }
        let resp = apply_migration(
            State(state.clone()),
            Json(ApplyRequest { overwrite: true }),
        )
        .await;
        match saved {
            Some(v) => unsafe { std::env::set_var("HERMIX_MIGRATION_PROBE_CANDIDATES", v) },
            None => unsafe { std::env::remove_var("HERMIX_MIGRATION_PROBE_CANDIDATES") },
        }
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(state.hermix_home.join("MEMORY.md")).unwrap(),
            "imported"
        );

        let _ = std::fs::remove_dir_all(&legacy);
    }
}
