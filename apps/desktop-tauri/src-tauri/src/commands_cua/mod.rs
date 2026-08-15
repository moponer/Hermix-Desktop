//! CUA Tauri commands — Phase 1 finishing.
//!
//! Five thin Tauri command surfaces that wrap the `hermix-cua`
//! primitives directly. Production LLM calls flow through
//! `hermix-tools` → `ToolHandler` → agent loop; these commands
//! exist for:
//!
//! 1. **Dev / smoke testing** — invoke `cua_screenshot` from the
//!    webview console, see the PNG render inline.
//! 2. **Future direct-invocation paths** — e.g. a "Take
//!    screenshot" button in the right sidebar that doesn't need to
//!    round-trip through the LLM agent loop.
//!
//! ## Cross-platform safety
//!
//! `hermix-cua::capture` / `action` are `#[cfg(windows)]` — every
//! command below has a non-Windows stub that returns
//! `Err("cua_* commands require Windows")`. This keeps `cargo check`
//! on macOS / Linux CI green without conditionalising the whole
//! desktop-tauri workspace.
//!
//! ## Shared state
//!
//! `SAFETY` (rate-limit + blacklisted-chord guard) and `TRACE`
//! (JSONL audit recorder) are process-wide `Lazy` statics — every
//! command goes through the same gate so a blacklisted
//! `ctrl+alt+delete` chord is refused by the Tauri command exactly
//! like it is by the LLM path.
//!
//! ## Concurrency
//!
//! `hermix-cua` is synchronous. We wrap each call in
//! `tokio::task::spawn_blocking` + a 10-second timeout so a stuck
//! `BitBlt` / `SendInput` cannot stall the Tauri runtime. The
//! timeout pattern mirrors the one in
//! `hermix-tools/src/tools/cua.rs`.

use std::time::Duration;

use base64::Engine;
use once_cell::sync::Lazy;
use serde::Serialize;

// -- Windows-only imports -----------------------------------------------------
#[cfg(windows)]
use hermix_cua::{
    safety::SafetyGuard,
    trace::TraceRecorder,
};

// ---------------------------------------------------------------------------
// Shared guards
// ---------------------------------------------------------------------------

/// Per-process safety guard. Same instance for every CUA command so
/// the rate-limiter and the chord-blacklist see a single
/// cross-command view.
#[cfg(windows)]
static SAFETY: Lazy<SafetyGuard> = Lazy::new(SafetyGuard::new);

/// Per-process JSONL trace recorder. Writes to
/// `<HERMIX_HOME>/cua-tauri-trace.jsonl` when the env var is set,
/// otherwise into the system temp dir. Falls back to the
/// no-op [`TraceRecorder::null`] when the file cannot be opened
/// (read-only install, sandbox, …).
#[cfg(windows)]
static TRACE: Lazy<TraceRecorder> = Lazy::new(|| {
    let path = trace_path();
    TraceRecorder::open(&path).unwrap_or_else(|_| TraceRecorder::null())
});

/// Per-call timeout. Mirrors the 10s budget
/// `hermix-tools/src/tools/cua.rs` uses for the LLM path. Past
/// this, we drop the `JoinHandle` (the OS input work may finish
/// in the background, but the user-facing call returns `Timeout`).
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(windows)]
fn trace_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("HERMIX_HOME") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p).join("cua-tauri-trace.jsonl");
        }
    }
    // Fallback: system temp dir. The desktop app's installer sets
    // HERMIX_HOME in production; this fallback exists for dev
    // runs (`cargo tauri dev`) and for the smoke-test scenario
    // where the env var is missing.
    std::env::temp_dir().join("hermix-cua-tauri-trace.jsonl")
}

// ---------------------------------------------------------------------------
// Shared runner
// ---------------------------------------------------------------------------

/// Race a synchronous `hermix-cua` closure against `ACTION_TIMEOUT`.
/// Returns `Err(CuaError::Timeout)` when the budget is exceeded;
/// otherwise passes through the closure's [`hermix_cua::CuaError`].
///
/// Always returns the native [`hermix_cua::CuaError`] so callers
/// can render a stable error code; the Tauri command boundary
/// `.map_err(|e| e.to_string())`s it for the wire.
#[cfg(windows)]
async fn run_blocking<F, R>(work: F) -> Result<R, hermix_cua::CuaError>
where
    F: FnOnce() -> Result<R, hermix_cua::CuaError> + Send + 'static,
    R: Send + 'static,
{
    match tokio::time::timeout(ACTION_TIMEOUT, tokio::task::spawn_blocking(work)).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(hermix_cua::CuaError::Gdi(format!(
            "cua handler panicked: {join_err}"
        ))),
        Err(_) => Err(hermix_cua::CuaError::Timeout(ACTION_TIMEOUT)),
    }
}

/// Cross-platform stub. Always errors on non-Windows targets so the
/// Tauri command surface is callable from JS but no-op. The work
/// closure is intentionally dropped without being run.
#[cfg(not(windows))]
async fn run_blocking<F, R>(_work: F) -> Result<R, hermix_cua::CuaError>
where
    F: FnOnce() -> Result<R, hermix_cua::CuaError> + Send + 'static,
    R: Send + 'static,
{
    Err(hermix_cua::CuaError::NotFound(
        "cua_* commands require Windows".into(),
    ))
}

// ---------------------------------------------------------------------------
// Shared response shape (so the front-end gets a stable JSON envelope)
// ---------------------------------------------------------------------------

/// Common response shape used by all five commands. `ok=true` is
/// the only state a successful command returns; `kind` is a
/// machine-readable discriminator so the front-end can render the
/// right card without parsing `output`.
///
/// camelCase on the wire to match the rest of the desktop-tauri
/// Tauri command surface (`ArtifactBytesContent`,
/// `OfficePreviewResult`, …) and the front-end `invoke<T>` generic
/// inference convention.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CuaCommandResponse {
    pub kind: String,
    pub ok: bool,
    pub elapsed_ms: u64,
    /// Structured payload. For `cua_screenshot` this is the
    /// serialised [`ScreenshotResult`] (with `png_base64`); for
    /// the others it's a small JSON object summarising the action.
    pub result: serde_json::Value,
}

impl CuaCommandResponse {
    fn ok(kind: &str, elapsed_ms: u64, result: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            ok: true,
            elapsed_ms,
            result,
        }
    }
}

// ===========================================================================
// 1) cua_screenshot
// ===========================================================================

/// Capture the primary monitor and return the PNG payload as
/// base64 alongside the frame dimensions.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_screenshot", { monitor: 0 })`.
///
/// The returned `result` field carries the full
/// [`ScreenshotResult`] (including `png_base64`). A 1920×1080
/// payload is ~1.5 MB of base64 — fine for a one-shot dev
/// invocation, but should not be polled in a tight loop.

pub mod screen;
pub mod window;
pub mod ui;
pub mod browser;

pub use screen::*;
pub use window::*;
pub use ui::*;
pub use browser::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_serialises_with_camel_case_keys() {
        // The front-end reads `kind`, `ok`, `elapsedMs`, `result`
        // (camelCase). Pin the rename so a future rename breaks the
        // test before it reaches the wire.
        let r = CuaCommandResponse::ok("cua_screenshot", 42, serde_json::json!({}));
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("kind").is_some());
        assert!(json.get("ok").is_some());
        assert!(
            json.get("elapsedMs").is_some(),
            "elapsed_ms Rust field must rename to elapsedMs on the wire"
        );
        assert!(json.get("result").is_some());
        // The Rust field name `elapsed_ms` must NOT leak onto the
        // wire (otherwise front-end `result.elapsedMs` is undefined).
        assert!(json.get("elapsed_ms").is_none());
    }
}

