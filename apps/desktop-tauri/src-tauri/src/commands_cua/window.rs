//! Window commands: enumerate, hit-test, click.
use super::*;

#[cfg(windows)]
use hermix_cua::{
    action::MouseButton,
    safety::RiskLevel,
    trace::{TraceEvent, TraceKind},
};

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
