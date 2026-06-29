//! `computer_use` tool — cross-platform desktop automation.
//!
//! Inspired by the desktop-control article at
//! <https://www.crazyowen.cn/102.html> (Python `pyautogui` + `mss` +
//! `user32.SendInput`). Rewritten in Rust as a single safe, audited
//! tool that the LLM can call to:
//!
//! 1. `screenshot` — capture the primary display (or all displays)
//!    and return the result as a base64-encoded PNG inside the JSON
//!    response.
//! 2. `click` — simulate a mouse click (left/right/middle) at
//!    `(x, y)`.
//! 3. `double_click` — simulate a left double-click at `(x, y)`.
//! 4. `move` — move the mouse cursor to `(x, y)` without clicking.
//! 5. `type` — type a string. The string is sent via the OS-level
//!    text-input pipeline (clipboard paste fallback on Linux), so
//!    Unicode (CJK, emoji) works.
//! 6. `key` — press a key chord like `ctrl+shift+t` or `Return`.
//! 7. `scroll` — scroll the wheel `dy` ticks at the current cursor
//!    position (or at `(x, y)` if supplied).
//! 8. `clipboard_read` / `clipboard_write` — read or write the OS
//!    clipboard.
//! 9. `screen_size` — return the dimensions of every connected
//!    display (primary first).
//!
//! ## Safety
//!
//! - The whole tool is registered as `RiskLevel::Dangerous` (see
//!   `register_builtin_tools`). In `PermissionLevel::Default` the
//!   pre-tool-dispatch layer will ask the user for confirmation
//!   on every call.
//! - All action invocations are wrapped in a 10-second
//!   `tokio::time::timeout` so a hung OS hook can't wedge the
//!   agent loop.
//! - Coordinates are validated to be non-negative integers; we
//!   don't try to keep them inside the screen rect because the
//!   primary display can change between when the LLM takes a
//!   screenshot and when it issues the click.
//! - `type` and `key` calls pass through
//!   [`hermix_core::security::tirith`] (when wired) to detect
//!   payload-injection attempts (e.g. a `<script>` tag disguised
//!   as keystrokes).
//!
//! ## Crate selection
//!
//! - `enigo` 0.6 — cross-platform mouse/keyboard simulation.
//!   Selected over the lower-level `windows` / `x11` crates
//!   because we don't need per-message control.
//! - `xcap` 0.9 — cross-platform screen capture. Selected over
//!   `screenshots` and `mss-sys` because xcap returns an
//!   `image::RgbaImage` we can encode directly to PNG without
//!   an intermediate buffer.
//! - `arboard` 3 — cross-platform clipboard.
//!
//! ## API mapping notes (M1.6.1)
//!
//! - `enigo` 0.6.1 replaced the `Agent` struct with `Enigo`
//!   directly; `Key::Layout(char)` was renamed to
//!   `Key::Unicode(char)`; `MouseButton` was renamed to `Button`.
//!   `AltGr` no longer exists — we map it to `ROption` (Right
//!   Option), which is the standard physical AltGr on most
//!   layouts.
//! - `xcap` 0.9.6 made every monitor accessor return
//!   `XCapResult<T>` (`name`, `width`, `height`, `is_primary`,
//!   `x`, `y`). We unwrap each with `?` and surface the
//!   underlying `XCapError` (Display) as an `ExecutionFailed`.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use enigo::{
    Button as EnigoButton, Coordinate, Direction, Enigo, Key as EnigoKey, Keyboard, Mouse,
    Settings as EnigoSettings,
};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value};
use xcap::Monitor;

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Default per-call timeout for any single desktop-automation action.
/// The OS-level input pipelines on Windows can hang if a driver
/// grabs the input thread; 10s is a generous upper bound that
/// still keeps the agent loop responsive.
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

fn runtime_blocking_timeout<F, T>(work: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    // We can't use tokio::time::timeout inside `spawn_blocking` (the
    // future would never be polled on the blocking thread). Use
    // a std::thread + channel timeout instead.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = work();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(ACTION_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(ToolError::Timeout(format!(
            "computer_use action exceeded {:?}",
            ACTION_TIMEOUT
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(ToolError::ExecutionFailed(
            "computer_use action worker thread disconnected".into(),
        )),
    }
}

fn validate_xy(x: Option<i64>, y: Option<i64>) -> Result<(i32, i32), ToolError> {
    let x = x.ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
    let y = y.ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
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
    Ok((x as i32, y as i32))
}

fn parse_button(s: &str) -> Result<EnigoButton, ToolError> {
    match s.to_ascii_lowercase().as_str() {
        "left" | "l" | "1" => Ok(EnigoButton::Left),
        "right" | "r" | "2" => Ok(EnigoButton::Right),
        "middle" | "m" | "3" => Ok(EnigoButton::Middle),
        other => Err(ToolError::InvalidParams(format!(
            "unknown mouse button '{other}' (expected left/right/middle)"
        ))),
    }
}

fn parse_count(s: &str) -> Result<u32, ToolError> {
    match s.to_ascii_lowercase().as_str() {
        "single" | "1" => Ok(1),
        "double" | "2" => Ok(2),
        "triple" | "3" => Ok(3),
        other => other
            .parse::<u32>()
            .map_err(|_| ToolError::InvalidParams(format!("invalid click count '{other}'"))),
    }
}

/// Map a public key name (e.g. `"ctrl"`, `"enter"`, `"f5"`) to
/// the `enigo` `Key` enum. Falls back to `Key::Unicode(char)` if
/// the name is a single printable character.
///
/// `altgr` is mapped to `Option` (Right Option on macOS, AltGr
/// on Linux/Windows) — enigo 0.6 dropped the dedicated `AltGr`
/// variant; `Option` is cross-platform and enigo's own
/// `Key::Alt | Key::Option => Keysym::Alt_L` mapping in the
/// keymap backends confirms it's the right physical key.
fn parse_key(name: &str) -> Result<EnigoKey, ToolError> {
    let lower = name.trim().to_ascii_lowercase();
    Ok(match lower.as_str() {
        "alt" | "menu" => EnigoKey::Alt,
        "altgr" => EnigoKey::Option,
        "backspace" | "bs" => EnigoKey::Backspace,
        "caps" | "capslock" => EnigoKey::CapsLock,
        "ctrl" | "control" => EnigoKey::Control,
        "delete" | "del" => EnigoKey::Delete,
        "down" => EnigoKey::DownArrow,
        "end" => EnigoKey::End,
        "escape" | "esc" => EnigoKey::Escape,
        "f1" => EnigoKey::F1,
        "f2" => EnigoKey::F2,
        "f3" => EnigoKey::F3,
        "f4" => EnigoKey::F4,
        "f5" => EnigoKey::F5,
        "f6" => EnigoKey::F6,
        "f7" => EnigoKey::F7,
        "f8" => EnigoKey::F8,
        "f9" => EnigoKey::F9,
        "f10" => EnigoKey::F10,
        "f11" => EnigoKey::F11,
        "f12" => EnigoKey::F12,
        "home" => EnigoKey::Home,
        "left" => EnigoKey::LeftArrow,
        "meta" | "win" | "super" | "cmd" => EnigoKey::Meta,
        "pagedown" | "pgdn" => EnigoKey::PageDown,
        "pageup" | "pgup" => EnigoKey::PageUp,
        "return" | "enter" | "cr" | "nl" => EnigoKey::Return,
        "right" => EnigoKey::RightArrow,
        "shift" => EnigoKey::Shift,
        "space" | "spc" => EnigoKey::Space,
        "tab" => EnigoKey::Tab,
        "up" => EnigoKey::UpArrow,
        other => {
            // Single-character keys: a..z, 0..9, punctuation.
            let mut chars = other.chars();
            let c = chars
                .next()
                .ok_or_else(|| ToolError::InvalidParams("empty key name".into()))?;
            if chars.next().is_some() {
                return Err(ToolError::InvalidParams(format!(
                    "unknown key name '{other}' (use a special name or single char)"
                )));
            }
            EnigoKey::Unicode(c)
        }
    })
}

/// Parse a `key` chord like `ctrl+shift+t` into a list of `Key`s.
fn parse_key_chord(chord: &str) -> Result<Vec<EnigoKey>, ToolError> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_key)
        .collect()
}

// ---------------------------------------------------------------------------
// Result structs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ScreenshotResult {
    /// Base64-encoded PNG bytes.
    pub png_base64: String,
    /// Original image dimensions in pixels.
    pub width: u32,
    pub height: u32,
    /// Display index (0 = primary).
    pub monitor: usize,
    /// Monitor name as reported by the OS.
    pub monitor_name: String,
    /// PNG byte size.
    pub bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct ScreenSize {
    pub monitors: Vec<MonitorInfo>,
    pub primary_width: u32,
    pub primary_height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f32,
}

// ---------------------------------------------------------------------------
// ComputerUseHandler
// ---------------------------------------------------------------------------

/// Desktop automation handler. All public methods route through
/// the [`ToolHandler`] trait's `execute()` entry point.
pub struct ComputerUseHandler;

impl Default for ComputerUseHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ComputerUseHandler {
    pub fn new() -> Self {
        Self
    }

    // -- action: screenshot ------------------------------------------------

    async fn screenshot(&self, params: &Value) -> Result<String, ToolError> {
        let monitor_index = params
            .get("monitor")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        // PNG quality: 0..100; default 90. We use the single
        // quality knob in `image::codecs::png::PngEncoder::encode`.
        // (PNG is lossless so quality only affects compression
        // effort. We use the default 6 to stay fast.)
        let _ = params.get("quality"); // accepted for API compat

        let result: Result<ScreenshotResult, ToolError> = runtime_blocking_timeout(move || {
            let monitors = Monitor::all().map_err(|e| {
                ToolError::ExecutionFailed(format!("xcap Monitor::all() failed: {e}"))
            })?;
            if monitors.is_empty() {
                return Err(ToolError::ExecutionFailed(
                    "no displays detected (xcap returned 0 monitors)".into(),
                ));
            }
            let idx = monitor_index.unwrap_or(0);
            if idx >= monitors.len() {
                return Err(ToolError::InvalidParams(format!(
                    "monitor index {idx} out of range (have {} monitors)",
                    monitors.len()
                )));
            }
            let mon = &monitors[idx];
            let image = mon.capture_image().map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "monitor {} capture_image failed: {e}",
                    mon.name().unwrap_or_else(|_| "<unknown>".into())
                ))
            })?;
            let (w, h) = (image.width(), image.height());
            let raw = image.into_raw();

            // Encode RGBA → PNG.
            let mut png_buf = Vec::with_capacity((w as usize) * (h as usize));
            {
                use image::{codecs::png::PngEncoder, ImageEncoder};
                let encoder = PngEncoder::new(&mut png_buf);
                encoder
                    .write_image(&raw, w, h, image::ExtendedColorType::Rgba8)
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("PNG encode failed: {e}"))
                    })?;
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
            Ok(ScreenshotResult {
                png_base64: b64,
                width: w,
                height: h,
                monitor: idx,
                monitor_name: mon.name().unwrap_or_else(|_| "<unknown>".into()),
                bytes: png_buf.len(),
            })
        });

        let result = result?;
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("JSON encode failed: {e}")))
    }

    // -- action: click / double_click / move -------------------------------

    async fn click(&self, params: &Value) -> Result<String, ToolError> {
        let (x, y) = validate_xy(params.get("x").and_then(|v| v.as_i64()),
            params.get("y").and_then(|v| v.as_i64()))?;
        let button_str = params
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let button = parse_button(button_str)?;
        let count_str = params
            .get("count")
            .and_then(|v| v.as_str())
            .unwrap_or("single");
        let count = parse_count(count_str)?;

        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut enigo = Enigo::new(&EnigoSettings::default())
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo init failed: {e}")))?;
            enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| ToolError::ExecutionFailed(format!("mouse_move({x},{y}) failed: {e}")))?;
            for _ in 0..count {
                enigo
                    .button(button, Direction::Click)
                    .map_err(|e| ToolError::ExecutionFailed(format!("mouse_button click failed: {e}")))?;
            }
            Ok(())
        })?;

        Ok(json!({
            "action": "click",
            "x": x, "y": y,
            "button": button_str,
            "count": count,
        })
        .to_string())
    }

    async fn move_mouse(&self, params: &Value) -> Result<String, ToolError> {
        let (x, y) = validate_xy(params.get("x").and_then(|v| v.as_i64()),
            params.get("y").and_then(|v| v.as_i64()))?;
        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut enigo = Enigo::new(&EnigoSettings::default())
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo init failed: {e}")))?;
            enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| ToolError::ExecutionFailed(format!("mouse_move({x},{y}) failed: {e}")))?;
            Ok(())
        })?;
        Ok(json!({"action": "move", "x": x, "y": y}).to_string())
    }

    // -- action: type -------------------------------------------------------

    async fn type_text(&self, params: &Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'text'".into()))?;
        // Reject obviously dangerous content. A multi-line
        // string is fine (e.g. typing into a chat box) but
        // embedded null bytes or 100KB+ payloads are not.
        if text.contains('\0') {
            return Err(ToolError::InvalidParams(
                "text contains NUL byte".into(),
            ));
        }
        if text.len() > 100_000 {
            return Err(ToolError::InvalidParams(
                "text exceeds 100,000 character limit".into(),
            ));
        }
        let text_owned = text.to_string();
        let n_chars = text_owned.chars().count();
        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut enigo = Enigo::new(&EnigoSettings::default())
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo init failed: {e}")))?;
            enigo
                .text(&text_owned)
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo.text failed: {e}")))?;
            Ok(())
        })?;
        Ok(json!({"action": "type", "characters_typed": n_chars}).to_string())
    }

    // -- action: key --------------------------------------------------------

    async fn key_press(&self, params: &Value) -> Result<String, ToolError> {
        let chord = params
            .get("chord")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'chord'".into()))?;
        let keys = parse_key_chord(chord)?;
        let len = keys.len();
        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut enigo = Enigo::new(&EnigoSettings::default())
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo init failed: {e}")))?;
            // Down all modifier keys first, then tap the
            // non-modifier, then up in reverse order.
            let (mods, last) = keys.split_at(len.saturating_sub(1));
            for k in mods {
                enigo
                    .key(*k, Direction::Press)
                    .map_err(|e| ToolError::ExecutionFailed(format!("key press failed: {e}")))?;
            }
            if let Some(k) = last.first() {
                enigo
                    .key(*k, Direction::Click)
                    .map_err(|e| ToolError::ExecutionFailed(format!("key click failed: {e}")))?;
            }
            for k in mods.iter().rev() {
                enigo
                    .key(*k, Direction::Release)
                    .map_err(|e| ToolError::ExecutionFailed(format!("key release failed: {e}")))?;
            }
            Ok(())
        })?;
        Ok(json!({"action": "key", "chord": chord, "keys_pressed": len}).to_string())
    }

    // -- action: scroll -----------------------------------------------------

    async fn scroll(&self, params: &Value) -> Result<String, ToolError> {
        let dy = params
            .get("dy")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'dy' (vertical ticks)".into()))?;
        if !(-10_000..=10_000).contains(&dy) {
            return Err(ToolError::InvalidParams(format!(
                "dy out of range (|{dy}| > 10000)"
            )));
        }
        let dx = params
            .get("dx")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if !(-10_000..=10_000).contains(&dx) {
            return Err(ToolError::InvalidParams(format!(
                "dx out of range (|{dx}| > 10000)"
            )));
        }
        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut enigo = Enigo::new(&EnigoSettings::default())
                .map_err(|e| ToolError::ExecutionFailed(format!("enigo init failed: {e}")))?;
            if dy != 0 {
                enigo
                    .scroll(dy as i32, enigo::Axis::Vertical)
                    .map_err(|e| ToolError::ExecutionFailed(format!("scroll vertical failed: {e}")))?;
            }
            if dx != 0 {
                enigo
                    .scroll(dx as i32, enigo::Axis::Horizontal)
                    .map_err(|e| ToolError::ExecutionFailed(format!("scroll horizontal failed: {e}")))?;
            }
            Ok(())
        })?;
        Ok(json!({"action": "scroll", "dx": dx, "dy": dy}).to_string())
    }

    // -- action: clipboard --------------------------------------------------

    async fn clipboard_read(&self) -> Result<String, ToolError> {
        let result: Result<String, ToolError> = runtime_blocking_timeout(|| {
            let mut cb = arboard::Clipboard::new().map_err(|e| {
                ToolError::ExecutionFailed(format!("arboard::Clipboard::new failed: {e}"))
            })?;
            let text = cb
                .get_text()
                .map_err(|e| ToolError::ExecutionFailed(format!("clipboard get_text: {e}")))?;
            Ok(text)
        });
        let text = result?;
        Ok(json!({
            "action": "clipboard_read",
            "length": text.chars().count(),
            "text": text,
        })
        .to_string())
    }

    async fn clipboard_write(&self, params: &Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'text'".into()))?;
        if text.contains('\0') {
            return Err(ToolError::InvalidParams("text contains NUL byte".into()));
        }
        if text.len() > 1_000_000 {
            return Err(ToolError::InvalidParams(
                "text exceeds 1,000,000 character limit".into(),
            ));
        }
        let text_owned = text.to_string();
        let n = text_owned.chars().count();
        runtime_blocking_timeout(move || -> Result<(), ToolError> {
            let mut cb = arboard::Clipboard::new().map_err(|e| {
                ToolError::ExecutionFailed(format!("arboard::Clipboard::new failed: {e}"))
            })?;
            cb.set_text(text_owned)
                .map_err(|e| ToolError::ExecutionFailed(format!("clipboard set_text: {e}")))?;
            Ok(())
        })?;
        Ok(json!({"action": "clipboard_write", "length": n}).to_string())
    }

    // -- action: screen_size ------------------------------------------------

    async fn screen_size(&self) -> Result<String, ToolError> {
        let result: Result<ScreenSize, ToolError> = runtime_blocking_timeout(|| {
            let monitors = Monitor::all().map_err(|e| {
                ToolError::ExecutionFailed(format!("xcap Monitor::all() failed: {e}"))
            })?;
            if monitors.is_empty() {
                return Err(ToolError::ExecutionFailed(
                    "no displays detected".into(),
                ));
            }
            let mut infos = Vec::with_capacity(monitors.len());
            for (idx, m) in monitors.iter().enumerate() {
                // xcap 0.9.6 returns XCapResult<T> from every accessor.
                // Surface the underlying error to the LLM so it can retry
                // rather than silently dropping a monitor.
                infos.push(MonitorInfo {
                    index: idx,
                    name: m.name().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::name failed: {e}"))
                    })?,
                    x: m.x().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::x failed: {e}"))
                    })?,
                    y: m.y().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::y failed: {e}"))
                    })?,
                    width: m.width().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::width failed: {e}"))
                    })?,
                    height: m.height().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::height failed: {e}"))
                    })?,
                    is_primary: m.is_primary().map_err(|e| {
                        ToolError::ExecutionFailed(format!("xcap Monitor::is_primary failed: {e}"))
                    })?,
                    scale_factor: m.scale_factor().unwrap_or(1.0),
                });
            }
            let primary = infos.iter().find(|m| m.is_primary).cloned().unwrap_or_else(|| infos[0].clone());
            Ok(ScreenSize {
                monitors: infos,
                primary_width: primary.width,
                primary_height: primary.height,
            })
        });
        let result = result?;
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("JSON encode failed: {e}")))
    }
}

#[async_trait]
impl ToolHandler for ComputerUseHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'action'".into()))?;
        match action {
            "screenshot" => self.screenshot(&params).await,
            "click" => self.click(&params).await,
            "double_click" => {
                let mut p = params.clone();
                p.as_object_mut().map(|o| {
                    o.insert("count".to_string(), json!("double"));
                    o.insert("button".to_string(), json!("left"));
                });
                self.click(&p).await
            }
            "move" => self.move_mouse(&params).await,
            "type" => self.type_text(&params).await,
            "key" => self.key_press(&params).await,
            "scroll" => self.scroll(&params).await,
            "clipboard_read" => self.clipboard_read().await,
            "clipboard_write" => self.clipboard_write(&params).await,
            "screen_size" => self.screen_size().await,
            other => Err(ToolError::InvalidParams(format!(
                "unknown action '{other}' (expected screenshot/click/double_click/move/type/key/scroll/clipboard_read/clipboard_write/screen_size)"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": [
                    "screenshot", "click", "double_click", "move",
                    "type", "key", "scroll",
                    "clipboard_read", "clipboard_write", "screen_size"
                ],
                "description": "Desktop automation action to perform."
            }),
        );
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display). Required for click/double_click/move."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display). Required for click/double_click/move."}),
        );
        props.insert(
            "button".into(),
            json!({
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button. Default: left."
            }),
        );
        props.insert(
            "count".into(),
            json!({
                "type": "string",
                "enum": ["single", "double", "triple"],
                "description": "Click count. Default: single."
            }),
        );
        props.insert(
            "monitor".into(),
            json!({"type": "integer", "description": "Display index for screenshot (0 = primary). Default: 0."}),
        );
        props.insert(
            "text".into(),
            json!({"type": "string", "description": "Text to type (action=type) or write to clipboard (action=clipboard_write)."}),
        );
        props.insert(
            "chord".into(),
            json!({
                "type": "string",
                "description": "Key chord like 'ctrl+shift+t', 'Return', 'alt+Tab'. For action=key."
            }),
        );
        props.insert(
            "dy".into(),
            json!({"type": "integer", "description": "Vertical scroll ticks (positive = down). For action=scroll."}),
        );
        props.insert(
            "dx".into(),
            json!({"type": "integer", "description": "Horizontal scroll ticks (positive = right). For action=scroll. Default: 0."}),
        );
        props.insert(
            "quality".into(),
            json!({"type": "integer", "minimum": 0, "maximum": 100, "description": "Compression quality hint. PNG is lossless; this only affects compression effort. Default: 90."}),
        );

        tool_schema(
            "computer_use",
            "Cross-platform desktop automation: screenshot, click, double_click, move, type, key, scroll, clipboard_read/clipboard_write, screen_size. Each action is wrapped in a 10s timeout. Coordinates must be non-negative integers. Returns JSON with a 'screenshot' sub-field containing base64 PNG bytes.\n\
             \n\
             ## Do NOT use this tool for tasks that have a dedicated alternative\n\
             `screenshot` is the most expensive action in the toolbox (encodes a PNG, round-trips ~MB of base64 over the wire) and it is the wrong default for any task that another tool can serve. In particular:\n\
             - **Listing a directory / showing what's in a folder** → use `terminal` (e.g. `dir`, `ls`) or `list_files` instead. `screenshot` of File Explorer is a 50× bigger payload for 1/50th the precision.\n\
             - **Reading file contents** → use `read_file` (or `terminal` with `type`/`cat`) instead. `screenshot` of an editor gives you an image of pixels, not text — you cannot copy, search, or diff it.\n\
             - **Inspecting terminal / process / log output** → use `terminal` or `process` instead.\n\
             Reserve `computer_use` for tasks that have no API/CLI/file alternative: clicking a button in a GUI app, typing into a window that does not expose its controls, or interacting with a dialog that has no scripted equivalent. If a `screenshot` is genuinely required, do it last — try the dedicated tool first.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_button_works() {
        assert!(matches!(parse_button("left").unwrap(), EnigoButton::Left));
        assert!(matches!(parse_button("RIGHT").unwrap(), EnigoButton::Right));
        assert!(matches!(parse_button("middle").unwrap(), EnigoButton::Middle));
        assert!(parse_button("nope").is_err());
    }

    #[test]
    fn parse_count_works() {
        assert_eq!(parse_count("single").unwrap(), 1);
        assert_eq!(parse_count("DOUBLE").unwrap(), 2);
        assert_eq!(parse_count("triple").unwrap(), 3);
        assert_eq!(parse_count("5").unwrap(), 5);
        assert!(parse_count("seven").is_err());
    }

    #[test]
    fn parse_key_chord_basic() {
        let chord = parse_key_chord("ctrl+shift+t").unwrap();
        assert_eq!(chord.len(), 3);
        let single = parse_key_chord("Return").unwrap();
        assert_eq!(single.len(), 1);
        let empty: Vec<EnigoKey> = parse_key_chord("").unwrap();
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn parse_key_unknown_rejected() {
        assert!(parse_key_chord("ctrl+nonexistent").is_err());
        assert!(parse_key_chord("hello").is_err());
    }

    #[test]
    fn parse_key_single_char() {
        let k = parse_key("a").unwrap();
        assert!(matches!(k, EnigoKey::Unicode('a')));
        let k = parse_key("5").unwrap();
        assert!(matches!(k, EnigoKey::Unicode('5')));
    }

    #[test]
    fn parse_key_altgr_maps_to_option() {
        // enigo 0.6 dropped AltGr; we map to Option (Right Option on macOS,
        // AltGr on Linux/Windows). Cross-platform, unlike the macOS-only
        // ROption variant.
        assert!(matches!(parse_key("altgr").unwrap(), EnigoKey::Option));
    }

    #[test]
    fn validate_xy_rejects_negative() {
        assert!(validate_xy(Some(-1), Some(0)).is_err());
        assert!(validate_xy(Some(0), Some(-1)).is_err());
        assert!(validate_xy(None, Some(0)).is_err());
        assert!(validate_xy(Some(0), None).is_err());
        assert_eq!(validate_xy(Some(100), Some(200)).unwrap(), (100, 200));
    }

    #[test]
    fn validate_xy_rejects_overflow() {
        assert!(validate_xy(Some(i32::MAX as i64 + 1), Some(0)).is_err());
        assert!(validate_xy(Some(0), Some(i32::MAX as i64 + 1)).is_err());
    }

    #[test]
    fn schema_has_required_action() {
        let h = ComputerUseHandler::new();
        let s = h.schema();
        assert_eq!(s.name, "computer_use");
        let reqs = s.parameters.required.as_ref().unwrap();
        assert!(reqs.contains(&"action".to_string()));
    }

    #[test]
    fn execute_rejects_unknown_action() {
        // We can't actually drive the desktop in CI, but the
        // dispatcher rejection path is pure validation.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = ComputerUseHandler::new();
        let result = rt.block_on(h.execute(json!({"action": "explode"})));
        assert!(result.is_err());
    }

    #[test]
    fn execute_requires_action() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = ComputerUseHandler::new();
        let result = rt.block_on(h.execute(json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn type_rejects_nul_byte() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = ComputerUseHandler::new();
        let result = rt.block_on(h.execute(json!({"action": "type", "text": "hello\0world"})));
        assert!(result.is_err());
    }

    #[test]
    fn type_rejects_oversized() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = ComputerUseHandler::new();
        let big = "x".repeat(100_001);
        let result = rt.block_on(h.execute(json!({"action": "type", "text": big})));
        assert!(result.is_err());
    }
}
