//! Build a [`hermix_focus::MemoryRouter`] from the bundled memory backends.
//!
//! The project relies solely on the built-in `EmbeddedMemoryBackend`
//! (SQLite-backed `memory.db`) for persistent memory. All external memory
//! plugins (Hindsight, Honcho, Mem0, RetainDB, ByteRover, OpenViking,
//! SuperMemory, Holographic) have been removed — they had no real write
//! hook and forcing `sync_turn()` would mis-use their SDKs.
//!
//! The router setup is invoked once per session from
//! [`crate::local_agent_service::LocalAgentService::spawn_agent_loop`].

use std::sync::Arc;

use hermix_focus::{DynMemoryLayer, MemoryLayer, MemoryRouter};

/// Build a [`MemoryRouter`] containing only the local embedded SQLite
/// backend (`<hermix_home>/data/memory.db`). The local embedded backend
/// has native `time_window` filtering via SQL.
pub fn all_plugins_as_router(session_id: &str, hermix_home: &str) -> MemoryRouter {
    let mut backends: Vec<DynMemoryLayer> = Vec::new();
    let mut embedded_handle: Option<Arc<hermix_focus::EmbeddedMemoryBackend>> = None;

    // Native SQLite time_window-filtered backend goes first.
    // Uses data_dir_in to resolve `<home>/data/memory.db` correctly
    // in both normal and portable modes.
    let db_path = hermix_config::paths::data_dir_in(hermix_home).join("memory.db");
    match hermix_focus::EmbeddedMemoryBackend::open(&db_path) {
        Ok(embedded) => {
            // Phase 17: bind the backend to the session so its
            // write-path hooks (`on_session_end`,
            // `record_turn`'s session_id tag) carry the right
            // identifier. Without this, the embedded backend
            // writes rows with `source='turn'` but no
            // `session:<id>` tag — making cross-session recall
            // harder to scope.
            embedded.initialize(session_id, hermix_home);
            let embedded_arc = Arc::new(embedded);
            embedded_handle = Some(embedded_arc.clone());
            backends.push(embedded_arc);
        }
        Err(e) => {
            // M1.6.5: was a silent `if let Ok(...)` drop until now —
            // the router would silently run with no backends and
            // the user had no idea why memory.db never grew. Make
            // the failure loud so it's visible in production logs.
            tracing::error!(
                target: "hermix.agent",
                session_id,
                db_path = %db_path.display(),
                error = %e,
                "MemoryRouter: EmbeddedMemoryBackend::open failed; \
                 memory.db writes will be lost"
            );
        }
    }

    match embedded_handle {
        Some(emb) => MemoryRouter::new_with_embedded(backends, emb),
        None => MemoryRouter::new(backends),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-06-26: use the OS-native temp dir instead of hardcoded
    /// `/tmp/...` Unix paths.
    fn temp_home_dir(label: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "hermix-memory-layer-adapter-test-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p); // clean leftover
        let _ = std::fs::create_dir_all(&p);
        p.to_string_lossy().into_owned()
    }

    /// Native SQLite backend is always wired (it owns the local
    /// memory.db under `<home>/data/`). Without it recall is broken.
    #[test]
    fn router_contains_embedded_sqlite() {
        let home = temp_home_dir("embeddedsqlite");
        let router = all_plugins_as_router("sess-test", &home);
        let names: Vec<String> = router
            .backend_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "embedded-sqlite"),
            "router missing embedded-sqlite; got {names:?}"
        );
    }

    /// Ensure the local SQLite backend is present.
    #[test]
    fn router_has_at_least_one_backend() {
        let home = temp_home_dir("onebackend");
        let router = all_plugins_as_router("sess-test", &home);
        let count = router.backend_count();
        assert!(
            count >= 1,
            "expected at least 1 backend (SQLite), got {count}"
        );
    }

    /// `backend_count()` must agree with `backend_names().len()` —
    /// they share the same internal slice and any drift is a bug.
    #[test]
    fn router_count_matches_names() {
        let home = temp_home_dir("count");
        let router = all_plugins_as_router("sess-test", &home);
        let count = router.backend_count();
        let names_len = router.backend_names().len();
        assert_eq!(count, names_len, "backend_count / backend_names drift");
    }

    /// Empty session_id must not panic — the router is built once
    /// per session; a malformed session id must still produce a
    /// usable router (the SQLite backend is session-agnostic).
    #[test]
    fn router_with_empty_session_id_succeeds() {
        let home = temp_home_dir("emptysession");
        let router = all_plugins_as_router("", &home);
        assert!(router.backend_count() >= 1);
    }

    /// M1.6.5: an unwritable parent directory (read-only mount or
    /// permission-denied path) must NOT silently produce an empty
    /// router — it must log loudly. Before this fix the
    /// `if let Ok(embedded) = ...` arm dropped the failure silently.
    ///
    /// 2026-06-26: on Windows there's no `/dev/null` equivalent.
    /// Use a real file path (not a directory) — `create_dir_all`
    /// on a path whose parent is a regular file fails with
    /// `NotADirectory` on Linux and `AccessDenied`/`AlreadyExists`
    /// on Windows.
    #[test]
    fn router_with_unwritable_home_logs_and_survives() {
        let mut blocker = std::env::temp_dir();
        blocker.push(format!(
            "hermix-test-blocker-{}.file",
            std::process::id()
        ));
        std::fs::write(&blocker, b"i am a regular file, not a directory").unwrap();
        let unwritable_home = blocker.to_string_lossy().into_owned();
        let router = all_plugins_as_router("sess-bad", &unwritable_home);
        // When the embedded backend can't open, the router may be
        // empty — that's expected. The key assertion is it doesn't
        // panic.
        let _ = std::fs::remove_file(&blocker);
        let _ = router.backend_count();
    }
}
