//! Action primitives — mouse + keyboard input.
//!
//! Public surface (`left_click`, `right_click`, `type_text`,
//! `press_key`) is **synchronous**. Callers inside the agent
//! loop must wrap these in `tokio::task::spawn_blocking` (the
//! `hermix-tools` wrappers do this via the
//! `runtime_blocking_timeout` helper borrowed from
//! `computer_use.rs`).
//!
//! All entry points consult the [`crate::SafetyGuard`] *before*
//! dispatching to the OS, so a blacklisted chord or rate-limit
//! hit returns a `Denied` error without ever reaching
//! `SendInput`.

pub mod sendinput;

#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, SetForegroundWindow, ShowWindow, SHOW_WINDOW_CMD,
};

use crate::error::CuaError;
use crate::safety::{RiskLevel, SafetyGuard};
use crate::trace::{TraceEvent, TraceKind, TraceRecorder};
use crate::CuaResult;

/// Bring `hwnd` to the foreground and, if it is currently
/// minimised, restore it first. Returns `Ok(())` on success and
/// `Err(NotFound)` when the window is no longer valid.
///
/// This is a *window-state* mutation (separate from the mouse /
/// keyboard input surface above) — it is grouped with the
/// action module because callers almost always want to focus a
/// window right before clicking into it.
#[cfg(windows)]
pub fn focus_window(hwnd: isize) -> CuaResult<()> {
    let h = HWND(hwnd as *mut _);
    unsafe {
        // SW_RESTORE = 9 — un-minimise before foregrounding so
        // SetForegroundWindow does not silently bring up a
        // minimised taskbar thumbnail.
        let _ = ShowWindow(h, SHOW_WINDOW_CMD(9));
        let _ = BringWindowToTop(h);
        let ok = SetForegroundWindow(h);
        if !bool::from(ok) {
            return Err(CuaError::Uia(format!(
                "SetForegroundWindow({hwnd:#x}) returned FALSE (window may be owned by another thread or already foreground)"
            )));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn focus_window(_hwnd: isize) -> CuaResult<()> {
    Err(CuaError::Uia("focus_window requires Windows".into()))
}

/// Mouse button. Mirrors the public string API the LLM sees in
/// the tool schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn parse(s: &str) -> CuaResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "l" | "1" => Ok(MouseButton::Left),
            "right" | "r" | "2" => Ok(MouseButton::Right),
            "middle" | "m" | "3" => Ok(MouseButton::Middle),
            other => Err(CuaError::InvalidParams(format!(
                "unknown mouse button '{other}' (expected left/right/middle)"
            ))),
        }
    }
}

/// Left / right / middle click at `(x, y)`. The cursor is moved
/// first (`SetCursorPos`), then `SendInput` issues a DOWN/UP
/// pair. A short sleep between DOWN and UP keeps RDP / VNC
/// sessions from collapsing the two into a single event.
pub fn click(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    x: i32,
    y: i32,
    button: MouseButton,
) -> CuaResult<()> {
    if x < 0 || y < 0 {
        return Err(CuaError::InvalidParams(format!(
            "coordinates must be non-negative (got x={x}, y={y})"
        )));
    }
    // Click is `Medium` risk — almost any GUI action can be
    // destructive in the wrong window, but most clicks are
    // reversible. The dispatch gate in `PermissionLevel::Default`
    // handles the user prompt.
    safety.check(None, RiskLevel::Medium)?;

    let started = std::time::Instant::now();
    let summary = format!(
        "x={x} y={y} button={}",
        match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        }
    );
    let result = sendinput::click(x, y, button);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::Click, session_id, &summary).with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::Click, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}

/// Type a string of Unicode text. The text is injected via
/// `SendInput` with `KEYEVENTF_UNICODE`, which sends one DOWN/UP
/// pair per codepoint (no dependency on the current keyboard
/// layout). CJK, emoji, and combining marks all work; null bytes
/// and 100,000+ character payloads are rejected.
pub fn type_text(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    text: &str,
) -> CuaResult<()> {
    if text.contains('\0') {
        return Err(CuaError::InvalidParams("text contains NUL byte".into()));
    }
    if text.chars().count() > 100_000 {
        return Err(CuaError::InvalidParams(
            "text exceeds 100,000 character limit".into(),
        ));
    }
    safety.check(None, RiskLevel::Medium)?;

    let started = std::time::Instant::now();
    let summary = format!("chars={}", text.chars().count());
    let owned = text.to_string();
    let result = sendinput::type_text(&owned);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::TypeText, session_id, &summary).with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::TypeText, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}

/// Press a key chord (e.g. `"ctrl+shift+t"`, `"Return"`,
/// `"alt+F4"`). The chord is parsed, normalised, and **checked
/// against the safety blacklist** before any input is injected.
/// Single-char names fall back to `KEYEVENTF_UNICODE` so the LLM
/// can call `press_key(chord="a")` for raw characters.
pub fn press_key(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    chord: &str,
) -> CuaResult<()> {
    // Reject empty / whitespace-only chords up front.
    if chord.trim().is_empty() {
        return Err(CuaError::InvalidParams("empty key chord".into()));
    }
    // `check` returns `Denied` for blacklisted chords — we do
    // NOT want to ever inject `ctrl+alt+delete` etc.
    safety.check(Some(chord), RiskLevel::High)?;

    let started = std::time::Instant::now();
    let summary = format!("chord={chord}");
    let owned = chord.to_string();
    let result = sendinput::press_key(&owned);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::PressKey, session_id, &summary).with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::PressKey, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}

/// Scroll the mouse wheel at `(x, y)` by `delta` clicks.
///
/// Positive `delta` scrolls up, negative scrolls down.
/// Each click is 120 units (WHEEL_DELTA).
pub fn scroll(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    x: i32,
    y: i32,
    delta: i32,
) -> CuaResult<()> {
    if x < 0 || y < 0 {
        return Err(CuaError::InvalidParams(format!(
            "coordinates must be non-negative (got x={x}, y={y})"
        )));
    }
    if delta == 0 {
        return Err(CuaError::InvalidParams("delta must not be zero".into()));
    }
    safety.check(None, RiskLevel::Medium)?;

    let started = std::time::Instant::now();
    let summary = format!("x={x} y={y} delta={delta}");
    let result = sendinput::scroll(x, y, delta);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::Other { subkind: "scroll".to_string() }, session_id, &summary)
            .with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::Other { subkind: "scroll".to_string() }, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}

/// Double-click at `(x, y)` with the left mouse button.
pub fn double_click(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    x: i32,
    y: i32,
) -> CuaResult<()> {
    if x < 0 || y < 0 {
        return Err(CuaError::InvalidParams(format!(
            "coordinates must be non-negative (got x={x}, y={y})"
        )));
    }
    safety.check(None, RiskLevel::Medium)?;

    let started = std::time::Instant::now();
    let summary = format!("x={x} y={y} button=left (double)");
    let result = sendinput::double_click(x, y);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::Click, session_id, &summary).with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::Click, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}

/// Move the mouse cursor to `(x, y)` without clicking.
pub fn move_cursor(
    safety: &SafetyGuard,
    trace: &TraceRecorder,
    session_id: &str,
    x: i32,
    y: i32,
) -> CuaResult<()> {
    if x < 0 || y < 0 {
        return Err(CuaError::InvalidParams(format!(
            "coordinates must be non-negative (got x={x}, y={y})"
        )));
    }
    // Move is Low risk — no click, just repositioning
    safety.check(None, RiskLevel::Low)?;

    let started = std::time::Instant::now();
    let summary = format!("x={x} y={y}");
    let result = sendinput::move_cursor(x, y);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ev = match &result {
        Ok(()) => TraceEvent::new(TraceKind::Other { subkind: "move_cursor".to_string() }, session_id, &summary)
            .with_elapsed_ms(elapsed_ms),
        Err(e) => TraceEvent::new(TraceKind::Other { subkind: "move_cursor".to_string() }, session_id, &summary)
            .with_elapsed_ms(elapsed_ms)
            .with_error(e),
    };
    trace.record(&ev)?;
    result
}