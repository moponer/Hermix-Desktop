//! Build a [`hermix_focus::MemoryRouter`] from the bundled memory plugins.
//!
//! After the 2026-06-21 cull, ByteRover is the only bundled plugin.
//! The seven retrieval-service stubs (Hindsight, Honcho, Mem0,
//! RetainDB, OpenViking, SuperMemory, Holographic) were removed —
//! they had no real write hook and forcing `sync_turn()` would
//! mis-use their SDKs (see `memory_plugins::mod` for the rationale).
//!
//! The router setup is invoked once per session from
//! [`crate::local_agent_service::LocalAgentService::spawn_agent_loop`].

use std::sync::Arc;

use hermix_focus::{DynMemoryLayer, MemoryLayer, MemoryRouter};

/// Build a [`MemoryRouter`] containing the local embedded SQLite
/// backend (`<hermix_home>/data/memory.db`) plus the ByteRover
/// plugin (when its `brv` CLI is on PATH). The local embedded
/// backend has native `time_window` filtering via SQL, so it goes
/// first; ByteRover is layered on top as an incremental recall
/// source.
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
            // the router would silently run with only ByteRover and
            // the user had no idea why memory.db never grew. Make
            // the failure loud so it's visible in production logs.
            tracing::error!(
                target: "hermix.agent",
                session_id,
                db_path = %db_path.display(),
                error = %e,
                "MemoryRouter: EmbeddedMemoryBackend::open failed; \
                 memory.db writes will fall through to plugins only"
            );
        }
    }

    // ByteRover is the only bundled plugin after the 2026-06-21
    // cull. Construct + initialize eagerly so the router has a
    // live backend from turn 0.
    for backend in native_plugin_backends(session_id, hermix_home) {
        backends.push(backend);
    }

    match embedded_handle {
        Some(emb) => MemoryRouter::new_with_embedded(backends, emb),
        None => MemoryRouter::new(backends),
    }
}

/// Iterate the bundled native `MemoryLayer` backends. After the
/// 2026-06-21 cull this is just ByteRover. Each plugin is
/// constructed fresh and `initialize()` is called with the session
/// id and `hermix_home` so it can set up its working directory.
fn native_plugin_backends(session_id: &str, hermix_home: &str) -> Vec<DynMemoryLayer> {
    use crate::memory_plugins::byterover::ByteRoverPlugin;

    let make = |backend: Arc<dyn MemoryLayer>| -> DynMemoryLayer { backend };

    vec![make(Arc::new({
        let p = ByteRoverPlugin::new();
        p.initialize(session_id, hermix_home);
        p
    }))]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-06-26: use the OS-native temp dir instead of hardcoded
    /// `/tmp/...` Unix paths. The previous tests were written for
    /// Linux/WSL where `/tmp` is writable; on Windows native + the
    /// sandbox env, `<refs>\...` is rejected and the embedded
    /// backend silently falls back to byterover-only, breaking the
    /// "2 backends" assertion. `std::env::temp_dir()` resolves to
    /// `%TEMP%` on Windows (a writable, sandbox-friendly path) and
    /// `/tmp` on Linux.
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

    /// Smoke test: the bundled plugin allowlist is wired in. After
    /// the 2026-06-21 cull the allowlist is just ByteRover.
    #[test]
    fn router_contains_byterover() {
        let home = temp_home_dir("byterover");
        let router = all_plugins_as_router("sess-test", &home);
        let names: Vec<String> = router
            .backend_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        // ByteRover is always wired (its initialize() is unconditional;
        // only its is_available() gates brv CLI presence). The router
        // itself doesn't care — it just addresses layers by name.
        assert!(
            names.iter().any(|n| n == "byterover"),
            "router missing byterover; got {names:?}"
        );
    }

    /// Ensure at least the local SQLite backend + ByteRover are
    /// present even when no `brv` CLI is on PATH.
    #[test]
    fn router_has_at_least_two_backends() {
        let home = temp_home_dir("twobackends");
        let router = all_plugins_as_router("sess-test", &home);
        let count = router.backend_count();
        assert!(
            count >= 2,
            "expected at least 2 backends (SQLite + ByteRover), got {count}"
        );
    }

    /// Native SQLite backend is always wired (it owns the local
    /// memory.db under `<home>/data/`). Without it recall degrades
    /// to byterover-only, which is wrong for the embedded scenario.
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

    /// The native SQLite backend must come first in the router —
    /// it has time_window SQL filtering, so it's the primary
    /// recall source. ByteRover layers on top incrementally.
    #[test]
    fn router_backend_order_is_sqlite_first() {
        let home = temp_home_dir("order");
        let router = all_plugins_as_router("sess-test", &home);
        let names: Vec<String> = router
            .backend_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let sqlite_pos = names.iter().position(|n| n == "embedded-sqlite");
        let byterover_pos = names.iter().position(|n| n == "byterover");
        match (sqlite_pos, byterover_pos) {
            (Some(s), Some(b)) => assert!(
                s < b,
                "embedded-sqlite must precede byterover; got order {names:?}"
            ),
            _ => panic!("both backends should be present; got {names:?}"),
        }
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
        assert!(router.backend_count() >= 2);
    }

    /// M1.6.5: an unwritable parent directory (read-only mount or
    /// permission-denied path) must NOT silently drop the embedded
    /// backend — it must log loudly and the router must still
    /// surface ByteRover so recall isn't completely broken. Before
    /// this fix the `if let Ok(embedded) = ...` arm dropped the
    /// failure silently.
    ///
    /// 2026-06-26: on Windows there's no `/dev/null` equivalent.
    /// Use a real file path (not a directory) — `create_dir_all`
    /// on a path whose parent is a regular file fails with
    /// `NotADirectory` on Linux and `AccessDenied`/`AlreadyExists`
    /// on Windows. The cross-platform pick: write a temp file and
    /// then ask `create_dir_all(<file-as-dir>)` to fail.
    #[test]
    fn router_with_unwritable_home_still_has_byterover_and_logs() {
        let mut blocker = std::env::temp_dir();
        blocker.push(format!(
            "hermix-test-blocker-{}.file",
            std::process::id()
        ));
        std::fs::write(&blocker, b"i am a regular file, not a directory").unwrap();
        let unwritable_home = blocker.to_string_lossy().into_owned();
        let router = all_plugins_as_router("sess-bad", &unwritable_home);
        let names: Vec<String> = router
            .backend_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let _ = std::fs::remove_file(&blocker);
        // ByteRover is unconditionally wired, so the router must
        // still have at least one backend (rather than being empty,
        // which would silently break all recall).
        assert!(
            names.iter().any(|n| n == "byterover"),
            "byterover must be present even when embedded open fails; got {names:?}"
        );
    }
}