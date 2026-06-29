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
    action::{self, MouseButton},
    capture::{self, CaptureConfig, MonitorInfo, ScreenshotResult},
    safety::{RiskLevel, SafetyGuard},
    trace::{TraceEvent, TraceKind, TraceRecorder},
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
#[tauri::command]
pub async fn cua_screenshot(monitor: Option<usize>) -> Result<CuaCommandResponse, String> {
    let monitor_idx = monitor.unwrap_or(0);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let config = CaptureConfig {
            monitor: monitor_idx,
        };
        let result: Result<ScreenshotResult, hermix_cua::CuaError> =
            run_blocking(move || capture::screenshot(config)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let result = result.map_err(|e| e.to_string())?;
        // Trace record — best-effort; never fail the call on a
        // recorder write error.
        let ev = TraceEvent::new(
            TraceKind::Screenshot,
            "tauri-direct",
            format!(
                "monitor={} {}x{}",
                result.monitor, result.width, result.height
            ),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::to_value(&result)
            .map_err(|e| format!("encode screenshot result: {e}"))?;
        Ok(CuaCommandResponse::ok("cua_screenshot", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = monitor_idx;
        Err("cua_screenshot requires Windows".into())
    }
}

// ===========================================================================
// 2) cua_screen_info
// ===========================================================================

/// Enumerate connected displays (Phase 1: primary only).
#[tauri::command]
pub async fn cua_screen_info() -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let result: Result<Vec<MonitorInfo>, hermix_cua::CuaError> =
            run_blocking(capture::screen_info::enumerate).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let list = result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::ScreenInfo,
            "tauri-direct",
            format!("monitors={}", list.len()),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::to_value(&list)
            .map_err(|e| format!("encode screen_info result: {e}"))?;
        Ok(CuaCommandResponse::ok("cua_screen_info", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        Err("cua_screen_info requires Windows".into())
    }
}

// ===========================================================================
// 3) cua_left_click
// ===========================================================================

/// Click at `(x, y)`. `button` defaults to `left`; `right` and
/// `middle` are accepted.
///
/// The safety guard runs first — blacklisted chords are refused
/// even though `click` doesn't take one (this is a defence-in-depth
/// measure for future extensions that reuse `click` internally
/// with a chord parameter).
#[tauri::command]
pub async fn cua_left_click(
    x: i32,
    y: i32,
    button: Option<String>,
) -> Result<CuaCommandResponse, String> {
    let button_str = button.unwrap_or_else(|| "left".into());
    #[cfg(windows)]
    {
        let button = MouseButton::parse(&button_str)
            .map_err(|e| format!("cua::invalid_params: {e}"))?;
        let started = std::time::Instant::now();
        let session_id = "tauri-direct".to_string();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            action::click(&SAFETY, &TRACE, &session_id, x, y, button)
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let value = serde_json::json!({
            "x": x,
            "y": y,
            "button": button_str,
        });
        Ok(CuaCommandResponse::ok("cua_left_click", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y, button_str);
        Err("cua_left_click requires Windows".into())
    }
}

// ===========================================================================
// 4) cua_type_text
// ===========================================================================

/// Type a string of Unicode text into whatever window currently
/// has focus. CJK, emoji, and combining marks all work
/// (KEYEVENTF_UNICODE). Empty / NUL / >100k-char payloads are
/// rejected by `hermix-cua::action::type_text`.
#[tauri::command]
pub async fn cua_type_text(text: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let session_id = "tauri-direct".to_string();
        let owned = text.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            action::type_text(&SAFETY, &TRACE, &session_id, &owned)
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let n_chars = text.chars().count();
        let value = serde_json::json!({
            "characters_typed": n_chars,
        });
        Ok(CuaCommandResponse::ok("cua_type_text", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err("cua_type_text requires Windows".into())
    }
}

// ===========================================================================
// 5) cua_press_key
// ===========================================================================

/// Press a key chord (e.g. `"ctrl+shift+t"`, `"Return"`,
/// `"alt+F4"`). Blacklisted chords (`ctrl+alt+delete`,
/// `win+l`, `alt+f4`, …) are refused before any input is
/// injected — the safety guard returns `Denied` and the Tauri
/// command surfaces a 4xx-style error string.
#[tauri::command]
pub async fn cua_press_key(chord: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let session_id = "tauri-direct".to_string();
        let owned = chord.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            action::press_key(&SAFETY, &TRACE, &session_id, &owned)
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let value = serde_json::json!({
            "chord": chord,
        });
        Ok(CuaCommandResponse::ok("cua_press_key", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = chord;
        Err("cua_press_key requires Windows".into())
    }
}

// ===========================================================================
// 6) cua_list_windows
// ===========================================================================

/// Enumerate top-level windows and return the matching `WindowInfo`
/// list. Mirrors the LLM-facing `cua_list_windows` tool semantics so
/// the dev console and the agent see the same shape.
///
/// Filters (all optional, all match the schema in
/// `hermix-tools/src/tools/cua.rs`):
/// - `title` — case-insensitive substring
/// - `visible_only` — default `true`
/// - `max_results` — default `100`
///
/// Front-end: `invoke<CuaCommandResponse>("cua_list_windows", {...})`.
///
/// The returned `result.windows` is a JSON array of `WindowInfo`
/// records (hwnd, title, class_name, process_id, is_visible, rect).
#[tauri::command]
pub async fn cua_list_windows(
    title: Option<String>,
    visible_only: Option<bool>,
    max_results: Option<usize>,
) -> Result<CuaCommandResponse, String> {
    let title_owned = title.clone();
    let visible = visible_only.unwrap_or(true);
    let cap = max_results.unwrap_or(100);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let filter = hermix_cua::element::WindowFilter {
            title: title_owned.clone(),
            class_name: None,
            process_id: None,
            visible_only: visible,
        };
        let cap_inner = cap;
        let result: Result<Vec<hermix_cua::element::WindowInfo>, hermix_cua::CuaError> =
            run_blocking(move || hermix_cua::element::find_all_windows(&filter)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let mut windows = result.map_err(|e| e.to_string())?;
        let truncated = windows.len() > cap_inner;
        if truncated {
            windows.truncate(cap_inner);
        }
        let n = windows.len();
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "list_windows".into(),
            },
            "tauri-direct",
            format!("matched={n} truncated={truncated} title={title_owned:?}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "matched": n,
            "truncated": truncated,
            "windows": windows,
        });
        Ok(CuaCommandResponse::ok("cua_list_windows", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (title_owned, visible, cap);
        Err("cua_list_windows requires Windows".into())
    }
}

// ===========================================================================
// 7) cua_window_at_point
// ===========================================================================

/// Return the top-level window under screen coordinate `(x, y)`.
/// `window` is `null` when the point hits nothing.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_window_at_point", { x, y })`.
#[tauri::command]
pub async fn cua_window_at_point(x: i32, y: i32) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        if x < 0 || y < 0 {
            return Err(format!(
                "cua::invalid_params: coordinates must be non-negative (got x={x}, y={y})"
            ));
        }
        let started = std::time::Instant::now();
        let result: Result<Option<hermix_cua::element::WindowInfo>, hermix_cua::CuaError> =
            run_blocking(move || hermix_cua::element::window_at_point(x, y)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let window = result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "window_at_point".into(),
            },
            "tauri-direct",
            format!("x={x} y={y} hit={}", window.is_some()),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "x": x,
            "y": y,
            "window": window,
        });
        Ok(CuaCommandResponse::ok("cua_window_at_point", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        Err("cua_window_at_point requires Windows".into())
    }
}

// ===========================================================================
// 8) cua_click_window
// ===========================================================================

/// Focus the window at `hwnd` and click its centre. `button` defaults
/// to `left`. The click coordinate is reported back in `result`
/// so the front-end can show the agent *exactly* where the click
/// landed.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_click_window", { hwnd, button? })`.
///
/// This combines `action::focus_window` + `action::click` — the
/// same two-step the LLM-facing `CuaClickWindowHandler` performs,
/// with the shared process-wide `SAFETY` guard and `TRACE` recorder
/// so dev-console invocations and agent invocations share the same
/// audit log and rate-limit budget.
#[tauri::command]
pub async fn cua_click_window(
    hwnd: isize,
    button: Option<String>,
) -> Result<CuaCommandResponse, String> {
    let button_str = button.unwrap_or_else(|| "left".into());
    #[cfg(windows)]
    {
        if hwnd <= 0 {
            return Err(format!(
                "cua::invalid_params: hwnd must be positive (got {hwnd})"
            ));
        }
        let button_enum = MouseButton::parse(&button_str)
            .map_err(|e| format!("cua::invalid_params: {e}"))?;
        let started = std::time::Instant::now();
        let session_id = "tauri-direct".to_string();
        let hwnd_copy = hwnd;
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let result: Result<(i32, i32), hermix_cua::CuaError> = run_blocking(move || {
            // Step 1 — validate the HWND.
            let win =
                hermix_cua::element::window_from_hwnd(hwnd_copy).ok_or_else(|| {
                    hermix_cua::CuaError::NotFound(format!(
                        "window hwnd={hwnd_copy:#x} is not valid"
                    ))
                })?;
            // Step 2 — focus it.
            hermix_cua::action::focus_window(hwnd_copy)?;
            // Step 3 — click its centre.
            let (cx, cy) = win.rect.center();
            if cx < 0 || cy < 0 {
                return Err(hermix_cua::CuaError::InvalidParams(format!(
                    "window hwnd={hwnd_copy:#x} has degenerate/offscreen rect ({cx},{cy})"
                )));
            }
            hermix_cua::action::click(safety, trace, &session_id, cx, cy, button_enum)?;
            Ok((cx, cy))
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (cx, cy) = result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "click_window".into(),
            },
            "tauri-direct",
            format!("hwnd={hwnd:#x} clicked_at=({cx},{cy}) button={button_str}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "hwnd": hwnd,
            "clicked_at": { "x": cx, "y": cy },
            "button": button_str,
        });
        Ok(CuaCommandResponse::ok("cua_click_window", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, button_str);
        Err("cua_click_window requires Windows".into())
    }
}

// ===========================================================================
// 9) cua_ui_tree
// ===========================================================================

/// Walk the UIA control-view tree up to `max_depth` levels and
/// return every element with its key properties (name, class name,
/// control type, bounding rect, enabled / focused flags, …).
///
/// Mirrors the LLM-facing `cua_ui_tree` tool semantics so the
/// dev console sees the same shape as the agent. The default
/// `max_depth` is **3** — desktop trees explode in size very
/// quickly (`{u32::MAX}` returns 10k+ elements and several
/// seconds; the LLM path is responsible for keeping depth low).
///
/// Each call constructs a fresh `UiaAutomation` instance inside
/// the blocking closure because the COM apartment is per-thread
/// and `spawn_blocking` gives us a fresh thread.
#[tauri::command]
pub async fn cua_ui_tree(max_depth: Option<u32>) -> Result<CuaCommandResponse, String> {
    let depth = max_depth.unwrap_or(3);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let result: Result<Vec<hermix_cua::uia::UiElement>, hermix_cua::CuaError> =
            run_blocking(move || {
                // Re-init UIA on the worker thread (COM apartment).
                let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
                unsafe { uia.get_control_view_tree(depth) }
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let elements = result.map_err(|e| e.to_string())?;
        let n = elements.len();
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_tree".into(),
            },
            "tauri-direct",
            format!("max_depth={depth} elements={n}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "max_depth": depth,
            "element_count": n,
            "elements": elements,
        });
        Ok(CuaCommandResponse::ok("cua_ui_tree", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = depth;
        Err("cua_ui_tree requires Windows".into())
    }
}

// ===========================================================================
// 10) cua_ui_find
// ===========================================================================

/// Find UIA elements matching `name`, `automation_id`, or
/// `class_name`. Multiple criteria are AND-ed together. Mirrors
/// the LLM-facing `cua_ui_find` tool. At least one criterion is
/// required.
#[tauri::command]
pub async fn cua_ui_find(
    name: Option<String>,
    automation_id: Option<String>,
    class_name: Option<String>,
    max_results: Option<usize>,
) -> Result<CuaCommandResponse, String> {
    if name.is_none() && automation_id.is_none() && class_name.is_none() {
        return Err(
            "cua::invalid_params: at least one of name/automation_id/class_name is required"
                .into(),
        );
    }
    let cap = max_results.unwrap_or(50);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let n_name = name.clone();
        let n_automation_id = automation_id.clone();
        let n_class_name = class_name.clone();
        let result: Result<Vec<hermix_cua::uia::UiElement>, hermix_cua::CuaError> =
            run_blocking(move || {
                use hermix_cua::uia::UiaCondition;
                let mut parts: Vec<UiaCondition> = Vec::new();
                if let Some(n) = n_name {
                    parts.push(UiaCondition::Name(n));
                }
                if let Some(a) = n_automation_id {
                    parts.push(UiaCondition::AutomationId(a));
                }
                if let Some(c) = n_class_name {
                    parts.push(UiaCondition::ClassName(c));
                }
                let cond = if parts.len() == 1 {
                    parts.into_iter().next().unwrap()
                } else {
                    UiaCondition::And(parts)
                };
                let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
                unsafe { uia.find_all(&cond) }
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let found = result.map_err(|e| e.to_string())?;
        let truncated = found.len() > cap;
        let slice: Vec<_> = found.into_iter().take(cap).collect();
        let n = slice.len();
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_find".into(),
            },
            "tauri-direct",
            format!(
                "name={name:?} auto_id={automation_id:?} class={class_name:?} found={n}"
            ),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "count": n,
            "truncated": truncated,
            "elements": slice,
        });
        Ok(CuaCommandResponse::ok("cua_ui_find", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (name, automation_id, class_name, cap);
        Err("cua_ui_find requires Windows".into())
    }
}

// ===========================================================================
// 11) cua_ui_click
// ===========================================================================

/// Find a UIA element by `name` and click its clickable point via
/// `SendInput`. Mirrors the LLM-facing `cua_ui_click` tool.
#[tauri::command]
pub async fn cua_ui_click(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = "tauri-direct".to_string();
        let closure_name = name.clone();
        let result: Result<(i32, i32), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            let (cx, cy) = unsafe { hermix_cua::uia::click_by_name(&uia, &closure_name) }?;
            hermix_cua::action::click(
                safety,
                trace,
                &session,
                cx,
                cy,
                hermix_cua::action::MouseButton::Left,
            )?;
            Ok((cx, cy))
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (cx, cy) = result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_click".into(),
            },
            "tauri-direct",
            format!("name={name} clicked_at=({cx},{cy})"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
            "clicked_at": { "x": cx, "y": cy },
        });
        Ok(CuaCommandResponse::ok("cua_ui_click", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_click requires Windows".into())
    }
}

// ===========================================================================
// 12) cua_ui_focus
// ===========================================================================

/// Find a UIA element by `name` and set keyboard focus via
/// `IUIAutomationElement::SetFocus`. Mirrors the LLM-facing
/// `cua_ui_focus` tool.
#[tauri::command]
pub async fn cua_ui_focus(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let closure_name = name.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            unsafe { hermix_cua::uia::focus_by_name(&uia, &closure_name) }
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_focus".into(),
            },
            "tauri-direct",
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
        });
        Ok(CuaCommandResponse::ok("cua_ui_focus", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_focus requires Windows".into())
    }
}

// ===========================================================================
// 13) cua_ui_invoke
// ===========================================================================

/// Find a UIA element by `name` and invoke its default action via
/// the `IUIAutomationInvokePattern`. Mirrors the LLM-facing
/// `cua_ui_invoke` tool.
#[tauri::command]
pub async fn cua_ui_invoke(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let closure_name = name.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            unsafe { hermix_cua::uia::invoke_by_name(&uia, &closure_name) }
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_invoke".into(),
            },
            "tauri-direct",
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
        });
        Ok(CuaCommandResponse::ok("cua_ui_invoke", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_invoke requires Windows".into())
    }
}

// ===========================================================================
// 14) cua_ocr_recognize
// ===========================================================================

/// Decode a base64-encoded PNG and run `Windows.Media.Ocr` over it.
/// Returns the full text plus per-line / per-word bounding rects.
///
/// Mirrors the LLM-facing `cua_ocr_recognize` tool. The decoded
/// bytes are moved into the blocking closure (single ownership);
/// the front-end just hands us the same `png_base64` it received
/// from `cua_screenshot`.
#[tauri::command]
pub async fn cua_ocr_recognize(png_base64: String) -> Result<CuaCommandResponse, String> {
    let png_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &png_base64,
    )
    .map_err(|e| format!("cua::invalid_params: base64 decode failed: {e}"))?;
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let result: Result<hermix_cua::ocr::OcrResponse, hermix_cua::CuaError> =
            run_blocking(move || hermix_cua::ocr::recognize_from_png(&png_bytes)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let resp = result.map_err(|e| e.to_string())?;
        let line_count = resp.lines.len();
        let preview = if resp.text.len() > 80 {
            format!("{}…", &resp.text[..80])
        } else {
            resp.text.clone()
        };
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ocr_recognize".into(),
            },
            "tauri-direct",
            format!("lines={line_count} preview={preview:?}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "text": resp.text,
            "lines": resp.lines,
            "line_count": line_count,
            "avg_confidence": resp.avg_confidence,
        });
        Ok(CuaCommandResponse::ok("cua_ocr_recognize", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = png_bytes;
        Err("cua_ocr_recognize requires Windows".into())
    }
}

// ===========================================================================
// Phase 2-E: Browser automation (self-hosted CDP client)
// ===========================================================================

/// Bootstrap a Chrome instance and navigate to `url`.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_navigate", { url })`.
#[tauri::command]
pub async fn cua_browser_navigate(url: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let url_c = url.clone();
        let result: Result<(String, String), String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::launch(
                    hermix_cua::browser::LaunchOptions::default(),
                )
                .or_else(|_| {
                    hermix_cua::browser::BrowserHandle::attach(
                        "ws://127.0.0.1:9222/devtools/browser",
                    )
                })
                .map_err(|e| format!("browser bootstrap failed: {e}"))?;

                let target_id = handle
                    .new_tab(&url_c)
                    .map_err(|e| format!("new_tab: {e}"))?;
                let session_id = handle
                    .attach_to_target(&target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;
                handle
                    .navigate(&session_id, &url_c)
                    .map_err(|e| format!("navigate: {e}"))?;

                Ok((target_id, session_id))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (_target_id, session_id) = result?;
        let value = serde_json::json!({
            "url": url,
            "sessionId": session_id,
        });
        Ok(CuaCommandResponse::ok("cua_browser_navigate", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("cua_browser_navigate requires Windows".into())
    }
}

/// Take a PNG screenshot of the first page tab via CDP.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_screenshot", {})`.
#[tauri::command]
pub async fn cua_browser_screenshot() -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let result: Result<Vec<u8>, String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                handle
                    .screenshot(&session_id, false)
                    .map_err(|e| format!("screenshot: {e}"))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let png_bytes = result?;
        let png_base64 =
            base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let value = serde_json::json!({
            "pngBase64": png_base64,
        });
        Ok(CuaCommandResponse::ok("cua_browser_screenshot", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        Err("cua_browser_screenshot requires Windows".into())
    }
}

/// Evaluate a JavaScript expression in the first page tab.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_evaluate", { expression })`.
#[tauri::command]
pub async fn cua_browser_evaluate(expression: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let expr = expression.clone();
        let result: Result<serde_json::Value, String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                handle
                    .evaluate(&session_id, &expr)
                    .map_err(|e| format!("evaluate: {e}"))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let value = result?;
        let payload = serde_json::json!({
            "result": value,
        });
        Ok(CuaCommandResponse::ok("cua_browser_evaluate", elapsed_ms, payload))
    }
    #[cfg(not(windows))]
    {
        let _ = expression;
        Err("cua_browser_evaluate requires Windows".into())
    }
}

/// Click a DOM element matching `selector` via `document.querySelector`.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_click", { selector })`.
#[tauri::command]
pub async fn cua_browser_click(selector: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let sel = selector.clone();
        let result: Result<(), String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                let js = format!("document.querySelector('{sel}').click()");
                handle
                    .evaluate(&session_id, &js)
                    .map_err(|e| format!("click: {e}"))?;
                Ok(())
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        result?;
        let value = serde_json::json!({
            "selector": selector,
        });
        Ok(CuaCommandResponse::ok("cua_browser_click", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = selector;
        Err("cua_browser_click requires Windows".into())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
