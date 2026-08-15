//! Screen & input commands: screenshot, screen info, click, type, key press.
use super::*;

#[cfg(windows)]
use hermix_cua::{
    action::{self, MouseButton},
    capture::{self, CaptureConfig, MonitorInfo, ScreenshotResult},
    safety::RiskLevel,
    trace::{TraceEvent, TraceKind},
};

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
