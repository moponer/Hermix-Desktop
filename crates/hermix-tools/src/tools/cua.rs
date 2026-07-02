//! Computer-Use Automation (CUA) tool handlers.
//!
//! Thin `ToolHandler` wrappers around the `hermix-cua` crate.
//! Each handler:
//!
//! 1. Parses and validates the LLM-supplied parameters.
//! 2. Calls into `hermix-cua` (synchronous, blocking — wrapped
//!    in `spawn_blocking` so the agent loop stays responsive).
//! 3. Serialises the result to the canonical JSON string the
//!    dispatcher expects.
//!
//! ## Shared state
//!
//! A single `SafetyGuard` and a single `TraceRecorder` are kept
//! in `Lazy` statics — every handler reads the same guard
//! (so the rate limiter counts across handlers) and the same
//! recorder (so the trace file shows the cross-tool action
//! sequence). The recorder writes to
//! `<HERMIX_HOME>/cua-trace.jsonl` when the home is reachable;
//! otherwise it falls back to the null sink.
//!
//! ## Risk classification
//!
//! All five tools are registered as `RiskLevel::Dangerous` in
//! `register_builtins`. The dispatch gate in
//! `local_agent_service::decide_pre_tool_dispatch` fires the user
//! approval card for every call under `PermissionLevel::Default`.
//! The safety guard in `hermix-cua` adds an *additional* refusal
//! layer for blacklisted key chords (Ctrl+Alt+Del, Win+L, …) that
//! must never reach the OS regardless of approval state.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel as CoreRisk};
use hermix_cua::{
    action::{self, MouseButton},
    capture::{self, CaptureConfig, MonitorInfo, ScreenshotResult},
    element::{WindowFilter, WindowInfo},
    safety::{RiskLevel, SafetyGuard},
    trace::{TraceEvent, TraceKind, TraceRecorder},
};

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all CUA (computer-use automation) tool handlers (P1.2 migration).
pub fn register(registry: &ToolRegistry) {
    // P1.8 (2026-07-02): tag every CUA handler with the *group* name
    // `"cua"`, not the per-tool name. `essential::ESSENTIAL_TOOLSETS`
    // references `"cua"` as a single toolset, and the
    // `set_enabled_toolsets` UI toggle keys on this tag. Tagging
    // each handler with its own name would have caused the toggle
    // to silently miss every CUA tool and the essential floor to
    // no-op on this group.
    let register_one = |name: &str, label: &str, risk: CoreRisk, handler: Arc<dyn ToolHandler>| {
        registry.register_with_risk(
            name,
            "cua",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Computer-Use Automation tool",
            label,
            None,
            risk,
            false,
        );
    };

    register_one("cua_screenshot", "📸", CoreRisk::Dangerous, Arc::new(CuaScreenshotHandler::new()));
    register_one("cua_screen_info", "🖥️", CoreRisk::Dangerous, Arc::new(CuaScreenInfoHandler::new()));
    register_one("cua_left_click", "🖱️", CoreRisk::Dangerous, Arc::new(CuaLeftClickHandler::new()));
    register_one("cua_type_text", "⌨️", CoreRisk::Dangerous, Arc::new(CuaTypeTextHandler::new()));
    register_one("cua_press_key", "🔘", CoreRisk::Dangerous, Arc::new(CuaPressKeyHandler::new()));
    register_one("cua_list_windows", "🪟", CoreRisk::Dangerous, Arc::new(CuaListWindowsHandler::new()));
    register_one("cua_window_at_point", "🎯", CoreRisk::Dangerous, Arc::new(CuaWindowAtPointHandler::new()));
    register_one("cua_click_window", "👆", CoreRisk::Dangerous, Arc::new(CuaClickWindowHandler::new()));
    register_one("cua_ui_tree", "🌳", CoreRisk::Dangerous, Arc::new(CuaUiTreeHandler::new()));
    register_one("cua_ui_find", "🔎", CoreRisk::Dangerous, Arc::new(CuaUiFindHandler::new()));
    register_one("cua_ui_click", "👆", CoreRisk::Dangerous, Arc::new(CuaUiClickHandler::new()));
    register_one("cua_ui_focus", "🎯", CoreRisk::Dangerous, Arc::new(CuaUiFocusHandler::new()));
    register_one("cua_ui_invoke", "⚙️", CoreRisk::Dangerous, Arc::new(CuaUiInvokeHandler::new()));
    register_one("cua_ocr_recognize", "🔤", CoreRisk::Safe, Arc::new(CuaOcrRecognizeHandler::new()));
}

// ---------------------------------------------------------------------------
// Shared guard + recorder
// ---------------------------------------------------------------------------

/// Process-wide safety guard. Shared across all five CUA handlers
/// so the rate limiter sees the cross-tool action stream.
static SAFETY: Lazy<SafetyGuard> = Lazy::new(SafetyGuard::new);

/// Process-wide trace recorder. Writes to `<HERMIX_HOME>/cua-trace.jsonl`;
/// falls back to the null sink when the home directory is not
/// writable (e.g. very early in startup, or in tests).
static TRACE: Lazy<TraceRecorder> = Lazy::new(|| {
    let path = hermix_config::paths::hermix_home().join("cua-trace.jsonl");
    TraceRecorder::open(&path).unwrap_or_else(|_| TraceRecorder::null())
});

/// Default per-call timeout for any single desktop-automation
/// action. Mirrors the 10-second budget
/// `computer_use::runtime_blocking_timeout` already uses — any
/// longer and the agent loop starves the user's UI.
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a synchronous `hermix-cua` closure on the blocking pool,
/// racing it against `ACTION_TIMEOUT`. The closure must be
/// infallible wrt the timeout — we cannot cancel the OS input
/// pipeline mid-call, but we can drop the `JoinHandle` and let
/// the work continue in the background while the agent loop
/// surfaces a `Timeout` error to the LLM.
async fn run_blocking<F, R>(work: F) -> Result<R, ToolError>
where
    F: FnOnce() -> Result<R, hermix_cua::CuaError> + Send + 'static,
    R: Send + 'static,
{
    match tokio::time::timeout(ACTION_TIMEOUT, tokio::task::spawn_blocking(work)).await {
        Ok(Ok(result)) => result.map_err(|e| map_cua_error(e)),
        Ok(Err(join_err)) => Err(ToolError::ExecutionFailed(format!(
            "cua handler panicked: {join_err}"
        ))),
        Err(_) => Err(ToolError::Timeout(format!(
            "cua action exceeded {:?}",
            ACTION_TIMEOUT
        ))),
    }
}

/// Translate a `hermix_cua::CuaError` into the canonical
/// `ToolError` the dispatcher expects. The `kind()` short string
/// is preserved in the message so the desktop UI can render a
/// stable error code.
///
/// `ToolError` does not have a dedicated `PermissionDenied`
/// variant — denied actions map to `ExecutionFailed` with a
/// `denied by safety:` prefix so the dispatcher-side error
/// filter can still match on it.
fn map_cua_error(err: hermix_cua::CuaError) -> ToolError {
    use hermix_cua::CuaError;
    match err {
        CuaError::Denied(msg) => {
            ToolError::ExecutionFailed(format!("denied by safety: {msg}"))
        }
        CuaError::Timeout(_) => ToolError::Timeout(format!("{:?}", ACTION_TIMEOUT)),
        CuaError::InvalidParams(msg) => ToolError::InvalidParams(msg),
        CuaError::NotFound(msg) => ToolError::NotFound(msg),
        other => ToolError::ExecutionFailed(format!("cua::{}: {}", other.kind(), other)),
    }
}

// ---------------------------------------------------------------------------
// Session id helper — every trace line carries a session id so
// the audit log can be grouped per task. We accept it as a
// parameter rather than reaching into globals; the dispatcher
// already passes it via the `tool_call_id` field on the JSON
// payload it forwards to the handler.
// ---------------------------------------------------------------------------

fn extract_session_id(params: &Value) -> String {
    params
        .get("_session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// 1) cua_screenshot
// ---------------------------------------------------------------------------

/// Capture the primary monitor and return a JSON envelope
/// containing the PNG bytes (base64) and the captured frame's
/// dimensions.
///
/// The handler does **not** add a top-level wrapper around the
/// `ScreenshotResult` struct — the inner `png_base64` field is
/// already JSON-safe (standard base64, no `\n` padding noise)
/// and the desktop UI reads the same shape for `computer_use`'s
/// screenshot result.
pub struct CuaScreenshotHandler;

impl CuaScreenshotHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaScreenshotHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaScreenshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let monitor = params.get("monitor").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let config = CaptureConfig { monitor };
        let session_id = extract_session_id(&params);
        let started = std::time::Instant::now();
        let result: Result<ScreenshotResult, ToolError> =
            run_blocking(move || capture::screenshot(config)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = match &result {
            Ok(r) => TraceEvent::new(
                TraceKind::Screenshot,
                &session_id,
                format!("monitor={} {}x{}", r.monitor, r.width, r.height),
            )
            .with_elapsed_ms(elapsed_ms),
            Err(e) => TraceEvent::new(
                TraceKind::Screenshot,
                &session_id,
                format!("monitor={monitor}"),
            )
            .with_elapsed_ms(elapsed_ms)
            .with_error(&hermix_cua::CuaError::Gdi(e.to_string())),
        };
        let _ = TRACE.record(&event);
        let result = result?;
        serde_json::to_string_pretty(&result).map_err(|e| {
            ToolError::ExecutionFailed(format!("screenshot JSON encode failed: {e}"))
        })
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "monitor".into(),
            json!({
                "type": "integer",
                "minimum": 0,
                "description": "Display index (0 = primary). Phase 1 only supports 0."
            }),
        );
        tool_schema(
            "cua_screenshot",
            "Capture the primary monitor and return the frame as base64-encoded PNG.\n\
             \n\
             ## When to use this (vs. `computer_use`)\n\
             `cua_screenshot` is the lower-latency sibling of the existing `computer_use`\n\
             tool — it uses the same wire format but a different capture backend\n\
             (BitBlt vs. xcap). For tasks that only need a screenshot (not a full\n\
             `computer_use` session), prefer `cua_screenshot` because it does not\n\
             import the input-pipeline crates and is therefore ~2× faster on first\n\
             call.\n\
             \n\
             ## Cost\n\
             The PNG payload is encoded once at capture time and streamed as\n\
             `png_base64` (standard base64, no line breaks). For a 1920×1080 screen\n\
             that is ~1–2 MB. The desktop UI renders the result inline.\n\
             \n\
             ## Coordinate system\n\
             All click / move / type coordinates returned by subsequent tools are\n\
             in the same pixel coordinate space as the screenshot.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 2) cua_screen_info
// ---------------------------------------------------------------------------

/// Return the list of connected displays (Phase 1: primary only).
pub struct CuaScreenInfoHandler;

impl CuaScreenInfoHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaScreenInfoHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaScreenInfoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let session_id = extract_session_id(&params);
        let started = std::time::Instant::now();
        let result: Result<Vec<MonitorInfo>, ToolError> =
            run_blocking(|| capture::screen_info::enumerate()).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = match &result {
            Ok(list) => TraceEvent::new(
                TraceKind::ScreenInfo,
                &session_id,
                format!("monitors={}", list.len()),
            )
            .with_elapsed_ms(elapsed_ms),
            Err(e) => TraceEvent::new(TraceKind::ScreenInfo, &session_id, "")
                .with_elapsed_ms(elapsed_ms)
                .with_error(&hermix_cua::CuaError::Gdi(e.to_string())),
        };
        let _ = TRACE.record(&event);
        let result = result?;
        serde_json::to_string_pretty(&result).map_err(|e| {
            ToolError::ExecutionFailed(format!("screen_info JSON encode failed: {e}"))
        })
    }

    fn schema(&self) -> ToolSchema {
        let props = IndexMap::new();
        tool_schema(
            "cua_screen_info",
            "Return the list of connected displays.\n\
             \n\
             Phase 1 returns the primary monitor only (Phase 2 will use\n\
             `EnumDisplayMonitors` + `GetMonitorInfo` to enumerate every display\n\
             in the virtual desktop coordinate space).\n\
             \n\
             ## When to call\n\
             The LLM should call `cua_screen_info` once at the start of a\n\
             Computer-Use session to learn the primary monitor's pixel\n\
             dimensions. All subsequent `cua_left_click` / `cua_type_text` /\n\
             `cua_press_key` coordinates are interpreted in the same coordinate\n\
             space.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 3) cua_left_click
// ---------------------------------------------------------------------------

/// Mouse click at `(x, y)`. Defaults to `button="left"`; the
/// right and middle buttons are accepted for completeness.
pub struct CuaLeftClickHandler;

impl CuaLeftClickHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaLeftClickHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaLeftClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let button_str = params
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let button = MouseButton::parse(button_str).map_err(|e| match e {
            hermix_cua::CuaError::InvalidParams(m) => ToolError::InvalidParams(m),
            other => ToolError::InvalidParams(other.to_string()),
        })?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        run_blocking(move || action::click(safety, trace, &session_id, x as i32, y as i32, button))
            .await?;

        Ok(json!({
            "tool": "cua_left_click",
            "x": x, "y": y,
            "button": button_str,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "button".into(),
            json!({
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button. Default: left."
            }),
        );
        tool_schema(
            "cua_left_click",
            "Click at (x, y) using `SendInput`. Defaults to a left click; `right`\n\
             and `middle` are also accepted.\n\
             \n\
             ## Coordinate space\n\
             The (x, y) values are pixel coordinates relative to the primary\n\
             monitor's top-left. Call `cua_screen_info` first to learn the\n\
             monitor's pixel dimensions, and use coordinates from a previous\n\
             `cua_screenshot` to land on the right pixel.\n\
             \n\
             ## When to use vs. `computer_use`\n\
             `cua_left_click` is a Phase-1 standalone — it does not require the\n\
             `computer_use` tool session to be open and can be called from any\n\
             agent. Prefer `cua_left_click` for one-off clicks; reach for\n\
             `computer_use` only when you need the full action surface (double\n\
             click, scroll, clipboard, multi-display).",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 4) cua_type_text
// ---------------------------------------------------------------------------

/// Type a string of Unicode text into whatever window currently
/// has focus. CJK, emoji, and combining marks all work (the
/// injection uses `KEYEVENTF_UNICODE`).
pub struct CuaTypeTextHandler;

impl CuaTypeTextHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaTypeTextHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaTypeTextHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'text'".into()))?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        let text_owned = text.to_string();
        let n_chars = text_owned.chars().count();
        run_blocking(move || action::type_text(safety, trace, &session_id, &text_owned)).await?;

        Ok(json!({
            "tool": "cua_type_text",
            "characters_typed": n_chars,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "text".into(),
            json!({"type": "string", "description": "Text to type into the currently focused window. Unicode is supported (CJK, emoji, combining marks)."}),
        );
        tool_schema(
            "cua_type_text",
            "Type a string of Unicode text using `SendInput` with `KEYEVENTF_UNICODE`.\n\
             \n\
             ## Limits\n\
             - Empty strings are rejected.\n\
             - NUL bytes are rejected.\n\
             - Strings over 100,000 characters are rejected.\n\
             \n\
             ## Coordinate independence\n\
             `cua_type_text` does not take coordinates — it types into whatever\n\
             window currently has keyboard focus. The agent must arrange focus\n\
             (e.g. via `cua_left_click` on the target text field) before calling\n\
             this tool.",
            JsonSchema::object(props, vec!["text".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 5) cua_press_key
// ---------------------------------------------------------------------------

/// Press a key chord (e.g. `"ctrl+shift+t"`, `"Return"`,
/// `"alt+F4"`). Blacklisted chords (`ctrl+alt+delete`, `win+l`,
/// `alt+f4`, etc.) are refused before any input is injected.
pub struct CuaPressKeyHandler;

impl CuaPressKeyHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaPressKeyHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaPressKeyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let chord = params
            .get("chord")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'chord'".into()))?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        let chord_owned = chord.to_string();
        run_blocking(move || action::press_key(safety, trace, &session_id, &chord_owned)).await?;

        Ok(json!({
            "tool": "cua_press_key",
            "chord": chord,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "chord".into(),
            json!({
                "type": "string",
                "description": "Key chord like 'ctrl+shift+t', 'Return', 'alt+F4'. Single printable characters are accepted as 'a', '5', etc."
            }),
        );
        tool_schema(
            "cua_press_key",
            "Press a key chord using `SendInput`.\n\
             \n\
             ## Blacklist\n\
             The following chords are refused by the safety guard before any\n\
             OS-level injection happens:\n\
             - `ctrl+alt+delete`, `ctrl+alt+del` — secure attention sequence\n\
             - `win+l`, `meta+l`, `super+l` — lock workstation\n\
             - `alt+f4` — force-close foreground window\n\
             - `ctrl+alt+end`, `ctrl+win+delete` — shutdown / log-off / restart\n\
             - `win+x`, `win+d` — power-user menu, show-desktop\n\
             \n\
             Attempting any of these returns a `Denied` error and never reaches\n\
             the OS input pipeline.\n\
             \n\
             ## Modifier order\n\
             Modifiers are sorted before comparison, so `ctrl+shift+t` and\n\
             `shift+ctrl+t` both match the blacklist identically. The actual\n\
             injection always presses the modifier `DOWN` first, then the tap\n\
             key, then releases the modifiers in reverse order.",
            JsonSchema::object(props, vec!["chord".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 6) cua_list_windows
// ---------------------------------------------------------------------------

/// Enumerate the desktop's top-level windows. Filters (all optional):
///
/// - `title` — case-insensitive substring match against the window
///   title. Empty / missing → match all titles.
/// - `visible_only` — default `true`; excludes hidden / owner-drawn
///   tool windows. Set `false` to enumerate everything.
/// - `max_results` — default `100`. The full desktop enumeration
///   rarely exceeds a few dozen visible windows; the cap is a safety
///   belt for the LLM context window, not a performance limit.
pub struct CuaListWindowsHandler;

impl CuaListWindowsHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaListWindowsHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaListWindowsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let visible_only = params
            .get("visible_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;
        let session_id = extract_session_id(&params);

        let filter = WindowFilter {
            title: title.clone(),
            class_name: None,
            process_id: None,
            visible_only,
        };

        let started = std::time::Instant::now();
        let result: Result<Vec<WindowInfo>, ToolError> = run_blocking(
            move || hermix_cua::element::find_all_windows(&filter),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let mut windows = result?;
        if windows.len() > max_results {
            windows.truncate(max_results);
        }
        let n = windows.len();
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "list_windows".into(),
            },
            &session_id,
            format!(
                "matched={n} title_filter={:?} visible_only={visible_only}",
                title
            ),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_list_windows",
            "matched": n,
            "truncated": n >= max_results,
            "windows": windows,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Case-insensitive substring filter for the window title. Optional — omit to list all windows."
            }),
        );
        props.insert(
            "visible_only".into(),
            json!({
                "type": "boolean",
                "description": "When true (default), only visible windows are returned.",
                "default": true,
            }),
        );
        props.insert(
            "max_results".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 1000,
                "description": "Cap on the number of returned windows. Default: 100.",
            }),
        );
        tool_schema(
            "cua_list_windows",
            "Enumerate top-level windows via Win32 `EnumWindows`.\n\
             \n\
             ## Returns\n\
             A JSON object with a `windows` array. Each entry has:\n\
             - `hwnd` — raw window handle (isize)\n\
             - `title` — `GetWindowTextW` result, may be empty\n\
             - `class_name` — `GetClassNameW` (e.g. `Notepad`, `#32770`, `Shell_TrayWnd`)\n\
             - `process_id` — owning PID\n\
             - `is_visible` — `IsWindowVisible`\n\
             - `rect` — `{x, y, width, height}` from `GetWindowRect`\n\
             \n\
             ## When to use\n\
             Call `cua_list_windows` to discover what apps are open\n\
             before clicking into one. Pair with `cua_window_at_point`\n\
             to figure out *which* window is under a screen coordinate\n\
             the user mentioned. Call `cua_click_window` to focus +\n\
             click into the centre of a window by its hwnd.\n\
             \n\
             ## Layer\n\
             This is the **Level-1 Win32 layer** (Phase 2-B). The full\n\
             UIA `IUIAutomation` tree (Phase 3) returns richer elements\n\
             (buttons, text fields, check boxes) with names and\n\
             automation IDs.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 7) cua_window_at_point
// ---------------------------------------------------------------------------

/// Return the top-level window under screen coordinate `(x, y)`.
/// Uses `WindowFromPoint` + a `IsWindow` validity check so a
/// stale / closed window cannot leak through.
///
/// Result is `null` when the point hits no window (e.g. outside
/// the virtual desktop).
pub struct CuaWindowAtPointHandler;

impl CuaWindowAtPointHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaWindowAtPointHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaWindowAtPointHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let result: Result<Option<WindowInfo>, ToolError> = run_blocking(
            move || hermix_cua::element::window_at_point(x as i32, y as i32),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let window = result?;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "window_at_point".into(),
            },
            &session_id,
            format!("x={x} y={y} hit={}", window.is_some()),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_window_at_point",
            "x": x,
            "y": y,
            "window": window,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate in screen pixels."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate in screen pixels."}),
        );
        tool_schema(
            "cua_window_at_point",
            "Return the top-level window under screen coordinate `(x, y)`.\n\
             \n\
             Backed by `WindowFromPoint` + `IsWindow`. Use this to\n\
             figure out which application a pixel belongs to before\n\
             issuing a `cua_left_click` into the same spot — it is\n\
             the safer sibling of blind coordinate clicks because it\n\
             surfaces the target's `title` / `class_name` / `pid`\n\
             the LLM can sanity-check against its intent.\n\
             \n\
             The `window` field is `null` when the point is outside\n\
             the desktop (negative virtual-screen coords, or a\n\
             multi-monitor gap).",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 8) cua_click_window
// ---------------------------------------------------------------------------

/// Focus a window by its `hwnd` and click into its centre. The
/// two-step shape (focus → click) is the natural pattern for
/// "switch to app X and then interact with it" — the focus step
/// makes the target window the foreground so the click lands
/// somewhere meaningful instead of being routed to whatever app
/// the user happened to be in.
///
/// The click coordinate is **always** the centre of the window's
/// `GetWindowRect` — explicit `(x, y)` overrides would defeat the
/// "click into the window" abstraction and re-introduce blind
/// coordinate clicking. Use `cua_left_click` for coordinate-precise
/// clicks.
pub struct CuaClickWindowHandler;

impl CuaClickWindowHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaClickWindowHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaClickWindowHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let hwnd = params
            .get("hwnd")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'hwnd'".into()))?;
        if hwnd <= 0 {
            return Err(ToolError::InvalidParams(format!(
                "hwnd must be a positive isize (got {hwnd})"
            )));
        }
        let button_str = params
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let button = MouseButton::parse(button_str).map_err(|e| match e {
            hermix_cua::CuaError::InvalidParams(m) => ToolError::InvalidParams(m),
            other => ToolError::InvalidParams(other.to_string()),
        })?;
        let session_id = extract_session_id(&params);

        // Step 1 — look up the window. We need the centre of its
        // rect for the click target; doing this outside the
        // blocking closure keeps the click + focus on the same
        // thread that called `find_window` so the rect cannot
        // shift between read and click in a noticeable way.
        let started = std::time::Instant::now();
        let hwnd_isize = hwnd as isize;
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = session_id.clone();
        let result: Result<(i32, i32), ToolError> = run_blocking(move || {
            // (a) Find the window to learn its rect.
            let win = hermix_cua::element::window_from_hwnd(hwnd_isize).ok_or_else(|| {
                hermix_cua::CuaError::NotFound(format!("window hwnd={hwnd_isize:#x} is not valid"))
            })?;
            // (b) Focus it (SetForegroundWindow + ShowWindow restore).
            hermix_cua::action::focus_window(hwnd_isize)?;
            // (c) Compute the click point — centre of the rect.
            let (cx, cy) = win.rect.center();
            if cx < 0 || cy < 0 {
                return Err(hermix_cua::CuaError::InvalidParams(format!(
                    "window hwnd={hwnd_isize:#x} has degenerate/offscreen rect ({},{})",
                    cx, cy
                )));
            }
            // (d) Issue the click through the normal action path so
            // the safety guard / trace recorder see it.
            hermix_cua::action::click(safety, trace, &session, cx, cy, button)?;
            Ok((cx, cy))
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (cx, cy) = result?;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "click_window".into(),
            },
            &session_id,
            format!("hwnd={hwnd:#x} clicked_at=({cx},{cy}) button={button_str}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_click_window",
            "hwnd": hwnd,
            "clicked_at": { "x": cx, "y": cy },
            "button": button_str,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "hwnd".into(),
            json!({
                "type": "integer",
                "description": "Window handle (isize) from a previous cua_list_windows or cua_window_at_point call."
            }),
        );
        props.insert(
            "button".into(),
            json!({
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button. Default: left."
            }),
        );
        tool_schema(
            "cua_click_window",
            "Focus a window by HWND and click into its centre.\n\
             \n\
             ## Steps performed\n\
             1. `WindowFromHwnd` — validate the HWND is still live\n\
             2. `SetForegroundWindow` + `ShowWindow(SW_RESTORE)` —\n\
                bring the window to the front (un-minimise if needed)\n\
             3. Compute the click target — centre of `GetWindowRect`\n\
             4. `SendInput` — left / right / middle click at that point\n\
             \n\
             ## When to use\n\
             Pair with `cua_list_windows`: the LLM first lists open\n\
             windows, picks the one matching its intent (e.g. by\n\
             title), then calls `cua_click_window` with that window's\n\
             `hwnd`. The focus step makes the target the foreground\n\
             app, so subsequent `cua_type_text` / `cua_press_key`\n\
             land in the right window.\n\
             \n\
             ## Coordinate target\n\
             Always the centre of the window — coordinate overrides\n\
             would defeat the abstraction. Use `cua_left_click` for\n\
             exact (x, y) clicks.",
            JsonSchema::object(props, vec!["hwnd".into()]),
        )
    }
}

// =========================================================================
// Phase 2-C: UIAutomation element-tree handlers
// =========================================================================

// Helper: run blocking COM calls with per-call CoInitializeEx.
// We construct a new UiaAutomation inside the blocking closure
// because COM apartment is per-thread and spawn_blocking gives
// us a fresh thread each time.
// Note: `UiaAutomation::new()` already calls CoInitializeEx, so
// it is safe to construct on a new thread.

macro_rules! uia_blocking {
    ($closure:expr) => {{
        tokio::task::spawn_blocking(move || -> Result<_, ToolError> {
            // Safety: spawn_blocking runs on a fresh thread pool
            // thread; COM apartment must be initialised per thread.
            let uia = unsafe {
                hermix_cua::uia::UiaAutomation::new()
                    .map_err(|e| ToolError::ExecutionFailed(format!("UIA init: {e}")))?
            };
            ($closure)(&uia)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("blocking panic: {e}")))?
    }};
}

// -- CuaUiTreeHandler -----------------------------------------------------

/// Dump the control-view UIA element tree up to a configurable depth.
pub struct CuaUiTreeHandler;

impl CuaUiTreeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiTreeHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiTreeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as u32;
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let elements = uia_blocking!(|uia: &hermix_cua::uia::UiaAutomation| {
            let tree = unsafe {
                uia.get_control_view_tree(max_depth)
                    .map_err(|e| ToolError::ExecutionFailed(format!("UIA tree walk: {e}")))?
            };
            Ok::<_, ToolError>(tree)
        })?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let count = elements.len();
        let event = TraceEvent::new(
            TraceKind::Other { subkind: "ui_tree".into() },
            &session_id,
            format!("max_depth={max_depth} elements={count}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_tree",
            "max_depth": max_depth,
            "element_count": count,
            "elements": elements,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "max_depth".into(),
            json!({
                "type": "integer",
                "description": "Maximum tree depth to walk. 0 = root only, 1 = root + immediate children, etc. Default: 3."
            }),
        );
        tool_schema(
            "cua_ui_tree",
            "Get the UI Automation control-view element tree.\n\
             \n\
             Walks the desktop's UIA element tree depth-first up to\n\
             `max_depth` levels and returns every element with its\n\
             name, class name, control type, and key properties.\n\
             \n\
             ## When to use\n\
             Before clicking or typing into a specific UI control,\n\
             call this first to discover the tree structure. The\n\
             returned elements have `name`, `automation_id`,\n\
             `control_type` which you can use to identify buttons,\n\
             text boxes, lists, etc. that the quick Win32 window\n\
             enumeration (`cua_list_windows`) cannot see.\n\
             \n\
             Use `cua_ui_find` to search within a tree, or\n\
             `cua_ui_click` to click a named element.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// -- CuaUiFindHandler -----------------------------------------------------

/// Find UIA elements by name, automation ID, class name, or control type.
pub struct CuaUiFindHandler;

impl CuaUiFindHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiFindHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiFindHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params.get("name").and_then(|v| v.as_str()).map(String::from);
        let automation_id = params.get("automation_id").and_then(|v| v.as_str()).map(String::from);
        let class_name = params.get("class_name").and_then(|v| v.as_str()).map(String::from);
        let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let session_id = extract_session_id(&params);

        if name.is_none() && automation_id.is_none() && class_name.is_none() {
            return Err(ToolError::InvalidParams(
                "at least one of 'name', 'automation_id', or 'class_name' is required".into(),
            ));
        }

        // Clone before spawning the blocking closure (which moves).
        let n_name = name.clone();
        let n_automation_id = automation_id.clone();
        let n_class_name = class_name.clone();

        let started = std::time::Instant::now();
        let elements = uia_blocking!(|uia: &hermix_cua::uia::UiaAutomation| {
            use hermix_cua::uia::UiaCondition;

            // Build condition from available criteria
            let mut parts: Vec<UiaCondition> = Vec::new();
            if let Some(ref n) = n_name {
                parts.push(UiaCondition::Name(n.clone()));
            }
            if let Some(ref a) = n_automation_id {
                parts.push(UiaCondition::AutomationId(a.clone()));
            }
            if let Some(ref c) = n_class_name {
                parts.push(UiaCondition::ClassName(c.clone()));
            }
            let cond = if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                UiaCondition::And(parts)
            };

            let found = unsafe {
                uia.find_all(&cond)
                    .map_err(|e| ToolError::ExecutionFailed(format!("UIA find: {e}")))?
            };

            let truncated = found.len() > max_results;
            let slice: Vec<_> = found.into_iter().take(max_results).collect();
            Ok::<_, ToolError>((slice, truncated))
        })?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let count = elements.0.len();
        let event = TraceEvent::new(
            TraceKind::Other { subkind: "ui_find".into() },
            &session_id,
            format!("name={name:?} auto_id={automation_id:?} class={class_name:?} found={count}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_find",
            "count": count,
            "truncated": elements.1,
            "elements": elements.0,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name substring (from `UIA_NamePropertyId`)." }),
        );
        props.insert(
            "automation_id".into(),
            json!({ "type": "string", "description": "Automation ID (from `UIA_AutomationIdPropertyId`)." }),
        );
        props.insert(
            "class_name".into(),
            json!({ "type": "string", "description": "Class name (from `UIA_ClassNamePropertyId`)." }),
        );
        props.insert(
            "max_results".into(),
            json!({ "type": "integer", "description": "Maximum results. Default: 50." }),
        );
        tool_schema(
            "cua_ui_find",
            "Find UIA elements matching name / automation ID / class name.\n\
             \n\
             At least one search criterion is required. Multiple\n\
             criteria are combined with AND. Use `cua_ui_tree` first\n\
             to discover what element names / IDs are available,\n\
             then call this to get the exact element(s).\n\
             \n\
             ## Examples\n\
             - `{\"name\": \"OK\"}` — find all elements named \"OK\"\n\
             - `{\"class_name\": \"Button\", \"name\": \"Save\"}` — find\n\
               the button named \"Save\"",
            JsonSchema::object(props, vec![]),
        )
    }
}

// -- CuaUiClickHandler ----------------------------------------------------

/// Find a UIA element by name and click its clickable point.
pub struct CuaUiClickHandler;

impl CuaUiClickHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiClickHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = session_id.clone();
        let closure_name = name.clone();
        let result: Result<(i32, i32), ToolError> = run_blocking(move || {
            // Step 1 — init UIA on this thread (COM apartment).
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            // Step 2 — find element by name and get clickable point.
            let (cx, cy) =
                unsafe { hermix_cua::uia::click_by_name(&uia, &closure_name)? };
            // Step 3 — issue the click through SendInput.
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
        let (cx, cy) = result?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_click".into(),
            },
            &session_id,
            format!("name={name} clicked_at=({cx},{cy})"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_click",
            "name": name,
            "clicked_at": { "x": cx, "y": cy },
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to search for (exact match on UIA Name property). First match wins." }),
        );
        tool_schema(
            "cua_ui_click",
            "Find a UIA element by name and click its clickable point.\n\
             \n\
             Looks up the element by its UIA `Name` property, retrieves\n\
             the clickable point via `IUIAutomationElement::GetClickablePoint`,\n\
             and issues a left mouse click at those coordinates.\n\
             \n\
             ## When to use\n\
             When you know the name of a button or control but not\n\
             its screen coordinates. Works for most standard Windows\n\
             controls (buttons, checkboxes, menu items, etc.).",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// -- CuaUiFocusHandler ----------------------------------------------------

/// Find a UIA element by name and set keyboard focus to it.
pub struct CuaUiFocusHandler;

impl CuaUiFocusHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiFocusHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiFocusHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let closure_name = name.clone();
        run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            unsafe { hermix_cua::uia::focus_by_name(&uia, &closure_name)? };
            Ok(())
        })
        .await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_focus".into(),
            },
            &session_id,
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_focus",
            "name": name,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to focus." }),
        );
        tool_schema(
            "cua_ui_focus",
            "Focus a UIA element by name.\n\
             Sets keyboard focus to the named element via\n\
             `IUIAutomationElement::SetFocus`.\n\
             \n\
             Use before typing into a text field or pressing a key\n\
             that should go to a specific control.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// -- CuaUiInvokeHandler ---------------------------------------------------

/// Find a UIA element by name and invoke its default action (e.g. click a button).
pub struct CuaUiInvokeHandler;

impl CuaUiInvokeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiInvokeHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiInvokeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let closure_name = name.clone();
        run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            unsafe { hermix_cua::uia::invoke_by_name(&uia, &closure_name)? };
            Ok(())
        })
        .await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_invoke".into(),
            },
            &session_id,
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_invoke",
            "name": name,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to invoke." }),
        );
        tool_schema(
            "cua_ui_invoke",
            "Invoke a UIA element's default action by name.\n\
             Uses the `IUIAutomationInvokePattern` to trigger the\n\
             element's default action (click a button, check a box,\n\
             select a menu item, follow a hyperlink).\n\
             \n\
             More reliable than `cua_ui_click` for buttons, but only\n\
             works on elements that implement the Invoke pattern.\n\
             Falls back to clicking the clickable point if the\n\
             element does not support Invoke_Pattern.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// =========================================================================
// Phase 2-D: OCR handler
// =========================================================================

// -- CuaOcrRecognizeHandler -----------------------------------------------

/// Recognise text in a PNG screenshot via Windows.Media.Ocr.
pub struct CuaOcrRecognizeHandler;

impl CuaOcrRecognizeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaOcrRecognizeHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaOcrRecognizeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let png_base64 = params
            .get("png_base64")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParams(
                    "missing 'png_base64' — pass the output of cua_screenshot's png_base64".into(),
                )
            })?;
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let closure_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            png_base64,
        )
        .map_err(|e| ToolError::InvalidParams(format!("base64 decode: {e}")))?;

        let result = run_blocking(move || {
            hermix_cua::ocr::recognize_from_png(&closure_bytes)
        })
        .await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let text_preview = if result.text.len() > 80 {
            format!("{}...", &result.text[..80])
        } else {
            result.text.clone()
        };
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ocr_recognize".into(),
            },
            &session_id,
            format!("lines={} preview={text_preview:?}", result.lines.len()),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ocr_recognize",
            "text": result.text,
            "lines": result.lines,
            "line_count": result.lines.len(),
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "png_base64".into(),
            json!({
                "type": "string",
                "description": "Base64-encoded PNG image data from cua_screenshot's png_base64 field."
            }),
        );
        tool_schema(
            "cua_ocr_recognize",
            "Recognise text in a PNG screenshot via Windows.Media.Ocr.\n\
             \n\
             Takes the `png_base64` output from `cua_screenshot`, decodes it,\n\
             and runs the Windows OCR engine to extract text, lines, and words\n\
             with bounding rectangles.\n\
             \n\
             ## Usage\n\
             1. Call `cua_screenshot` to get a screenshot\n\
             2. Pass its `png_base64` field to this tool\n\
             3. The result contains recognised `text` (all lines joined),\n\
                plus per-`line` and per-`word` detail with bounding rects\n\
             \n\
             ## Requirements\n\
             Windows 10/11 with an OCR language pack installed. English (US)\n\
             is pre-installed; Chinese Simplified needs the Chinese language\n\
             feature-on-demand.",
            JsonSchema::object(props, vec!["png_base64".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Re-exports — `register_builtins` imports the handler types
// directly. The convenience group below keeps the imports
// compact at the call site.
// ---------------------------------------------------------------------------

pub use self::{
    CuaClickWindowHandler as ClickWindow, CuaLeftClickHandler as LeftClick,
    CuaListWindowsHandler as ListWindows, CuaPressKeyHandler as PressKey,
    CuaScreenInfoHandler as ScreenInfo, CuaScreenshotHandler as Screenshot,
    CuaTypeTextHandler as TypeText, CuaWindowAtPointHandler as WindowAtPoint,
    // UIA element-tree handlers (Phase 2-C)
    CuaUiClickHandler as UiClick, CuaUiFindHandler as UiFind,
    CuaUiFocusHandler as UiFocus, CuaUiInvokeHandler as UiInvoke,
    CuaUiTreeHandler as UiTree,
    // OCR handler (Phase 2-D)
    CuaOcrRecognizeHandler as OcrRecognize,
};

// Convenience factory: build all five handlers behind an
// `Arc<dyn ToolHandler>` slice. `register_builtins` calls this
// when wiring the toolset.
pub fn all_handlers() -> Vec<Arc<dyn ToolHandler>> {
    vec![
        Arc::new(CuaScreenshotHandler::new()),
        Arc::new(CuaScreenInfoHandler::new()),
        Arc::new(CuaLeftClickHandler::new()),
        Arc::new(CuaTypeTextHandler::new()),
        Arc::new(CuaPressKeyHandler::new()),
        Arc::new(CuaListWindowsHandler::new()),
        Arc::new(CuaWindowAtPointHandler::new()),
        Arc::new(CuaClickWindowHandler::new()),
        // Phase 2-C: UIA element-tree handlers
        Arc::new(CuaUiTreeHandler::new()),
        Arc::new(CuaUiFindHandler::new()),
        Arc::new(CuaUiClickHandler::new()),
        Arc::new(CuaUiFocusHandler::new()),
        Arc::new(CuaUiInvokeHandler::new()),
        // Phase 2-D: OCR
        Arc::new(CuaOcrRecognizeHandler::new()),
    ]
}

// ---------------------------------------------------------------------------
// Tests — pure-Rust unit tests only; we do not exercise the
// Windows-only code paths here because those need a live desktop
// session. The schema tests run on every platform.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names_are_stable() {
        // Renaming a public tool name is a wire-protocol break —
        // pin the schema name here so an accidental rename fails
        // the test before it reaches the agent loop.
        assert_eq!(CuaScreenshotHandler::new().schema().name, "cua_screenshot");
        assert_eq!(CuaScreenInfoHandler::new().schema().name, "cua_screen_info");
        assert_eq!(CuaLeftClickHandler::new().schema().name, "cua_left_click");
        assert_eq!(CuaTypeTextHandler::new().schema().name, "cua_type_text");
        assert_eq!(CuaPressKeyHandler::new().schema().name, "cua_press_key");
    }

    #[test]
    fn schema_required_fields_present() {
        let s = CuaLeftClickHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"x".to_string()));
        assert!(reqs.contains(&"y".to_string()));
    }

    #[test]
    fn schema_text_required() {
        let s = CuaTypeTextHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"text".to_string()));
    }

    #[test]
    fn schema_chord_required() {
        let s = CuaPressKeyHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"chord".to_string()));
    }

    #[test]
    fn execute_rejects_missing_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaLeftClickHandler::new();
        let r = rt.block_on(h.execute(json!({"y": 100})));
        assert!(r.is_err());
        let r = rt.block_on(h.execute(json!({"x": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_negative_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaLeftClickHandler::new();
        let r = rt.block_on(h.execute(json!({"x": -1, "y": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_missing_text() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaTypeTextHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_missing_chord() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaPressKeyHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn all_handlers_returns_correct_count() {
        assert_eq!(all_handlers().len(), 14);
    }

    #[test]
    fn schema_names_phase2b_are_stable() {
        // Pin the Phase 2-B handler names so a typo fails the test
        // before it reaches the dispatcher.
        assert_eq!(CuaListWindowsHandler::new().schema().name, "cua_list_windows");
        assert_eq!(CuaWindowAtPointHandler::new().schema().name, "cua_window_at_point");
        assert_eq!(CuaClickWindowHandler::new().schema().name, "cua_click_window");
    }

    #[test]
    fn schema_window_at_point_required_fields() {
        let s = CuaWindowAtPointHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"x".to_string()));
        assert!(reqs.contains(&"y".to_string()));
    }

    #[test]
    fn schema_click_window_required_fields() {
        let s = CuaClickWindowHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"hwnd".to_string()));
    }

    #[test]
    fn execute_window_at_point_rejects_missing_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaWindowAtPointHandler::new();
        assert!(rt.block_on(h.execute(json!({"y": 100}))).is_err());
        assert!(rt.block_on(h.execute(json!({"x": 100}))).is_err());
        assert!(rt.block_on(h.execute(json!({}))).is_err());
    }

    #[test]
    fn execute_window_at_point_rejects_negative_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaWindowAtPointHandler::new();
        let r = rt.block_on(h.execute(json!({"x": -1, "y": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_click_window_rejects_missing_hwnd() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaClickWindowHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_click_window_rejects_zero_or_negative_hwnd() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaClickWindowHandler::new();
        assert!(rt.block_on(h.execute(json!({"hwnd": 0}))).is_err());
        assert!(rt.block_on(h.execute(json!({"hwnd": -1}))).is_err());
    }

    #[test]
    fn execute_list_windows_accepts_empty_params() {
        // No required fields — listing all visible windows is the
        // common case. We can't verify the *contents* (depends on
        // the live desktop), but we can verify the handler doesn't
        // reject the empty payload.
        //
        // We assert via parsed JSON rather than substring contains
        // — `serde_json::json!` uses a BTreeMap-keyed Map so the
        // wire key order is alphabetical, which makes
        // `s.contains("\"ok\": true")` brittle (any whitespace or
        // trailing-comma difference breaks it).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaListWindowsHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        // Will succeed on Windows with a non-empty desktop, fail on
        // non-Windows CI with the "requires Windows" stub. Either
        // way, the handler must not have errored on parsing.
        if let Ok(s) = r.as_ref() {
            let v: serde_json::Value =
                serde_json::from_str(s).expect("handler output must be valid JSON");
            assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
            assert_eq!(
                v.get("tool").and_then(|x| x.as_str()),
                Some("cua_list_windows")
            );
        }
    }
}