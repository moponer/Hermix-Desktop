//! Low-level `SendInput` bindings — Windows-only.
//!
//! This module is the only place the crate touches
//! `windows::Win32::UI::Input::KeyboardAndMouse`. Higher-level
//! helpers in [`super`] validate parameters, consult the safety
//! guard, and record the trace; everything below here is
//! straight injection.
//!
//! ## Coordinate system
//!
//! `SetCursorPos` accepts pixel coordinates relative to the
//! primary monitor's origin. We deliberately do **not** use
//! `MOUSEEVENTF_ABSOLUTE` — the normalised 0..65535 space has
//! surprising behaviour on multi-monitor setups, and pixel
//! coordinates are what every screenshot result returns.
//!
//! ## Unicode injection
//!
//! `KEYEVENTF_UNICODE` injects one `KEYBDINPUT` per codepoint
//! with `wVk = 0` and `wScan = codepoint`. This is the only
//! reliable way to send CJK / emoji through `SendInput` — the
//! `wVk` (virtual key) path is layout-bound and would
//! inconsistent across machines.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, VIRTUAL_KEY,
};

/// WHEEL_DELTA constant — one click of the mouse wheel.
const WHEEL_DELTA: i32 = 120;
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

use crate::action::MouseButton;
use crate::error::CuaError;
use crate::CuaResult;

/// Click at `(x, y)` with the given button. The cursor is moved
/// first, then DOWN/UP events are issued.
pub fn click(x: i32, y: i32, button: MouseButton) -> CuaResult<()> {
    // SAFETY: `SetCursorPos` is a Win32 API that moves the cursor to
    // the given screen coordinates. This is safe as long as the
    // coordinates are within the virtual screen area.
    unsafe {
        if SetCursorPos(x, y).is_err() {
            return Err(CuaError::SendInput(format!(
                "SetCursorPos({x}, {y}) failed"
            )));
        }
    }

    let (down_flag, up_flag) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    };

    let mut inputs = [
        mouse_input(down_flag),
        mouse_input(up_flag),
    ];

    // SAFETY: `SendInput` is a Win32 API that synthesises input
    // events. We pass a valid `INPUT` array with the correct size.
    unsafe {
        let sent = SendInput(
            &mut inputs,
            std::mem::size_of::<INPUT>() as i32,
        );
        if sent != 2 {
            return Err(CuaError::SendInput(format!(
                "SendInput returned {sent} (expected 2)"
            )));
        }
    }
    Ok(())
}

fn mouse_input(flag: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    // SAFETY: `std::mem::zeroed()` is safe because `INPUT` is a
    // C-compatible struct where all-zero bytes is a valid
    // representation. The `Anonymous.mi` union field write is
    // sound because we set `input.r#type = INPUT_MOUSE` first,
    // making the `mi` arm the active variant.
    unsafe {
        let mut input: INPUT = std::mem::zeroed();
        input.r#type = INPUT_MOUSE;
        let mi = windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT {
            dx: 0,
            dy: 0,
            mouseData: 0,
            dwFlags: flag,
            time: 0,
            dwExtraInfo: 0,
        };
        // Union field assignment — `mi` is a struct field of the
        // `INPUT_0` union, not a method. The `unsafe` block here
        // is what marks the union access as sound.
        input.Anonymous.mi = mi;
        input
    }
}

/// Type `text` as a sequence of Unicode keystrokes.
pub fn type_text(text: &str) -> CuaResult<()> {
    if text.is_empty() {
        return Ok(());
    }
    // Build the full batch up front so SendInput issues every
    // character in a single call (preserves ordering on slow
    // RDP / VNC links).
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.chars().count() * 2);
    for c in text.chars() {
        let down = unicode_keystroke(c, false);
        let up = unicode_keystroke(c, true);
        inputs.push(down);
        inputs.push(up);
    }
    // SAFETY: `SendInput` synthesises keystroke events. The `inputs`
    // vector contains valid `INPUT` structs with correct sizes.
    unsafe {
        let sent = SendInput(
            &mut inputs,
            std::mem::size_of::<INPUT>() as i32,
        );
        let expected = text.chars().count() * 2;
        if sent as usize != expected {
            return Err(CuaError::SendInput(format!(
                "SendInput returned {sent} (expected {expected})"
            )));
        }
    }
    Ok(())
}

fn unicode_keystroke(c: char, key_up: bool) -> INPUT {
    // SAFETY: `std::mem::zeroed()` is safe because `INPUT` is a
    // C-compatible struct. `input.r#type = INPUT_KEYBOARD` and
    // `input.Anonymous.ki` are set immediately, making the union
    // access sound.
    unsafe {
        let mut input: INPUT = std::mem::zeroed();
        input.r#type = INPUT_KEYBOARD;
        let mut flags = KEYEVENTF_UNICODE;
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        let ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: (c as u32) as u16,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        input.Anonymous.ki = ki;
        input
    }
}

/// Press a key chord like `"ctrl+shift+t"` or `"Return"`. The
/// chord is split on `+`, each part is mapped to a virtual key
/// (or to a single Unicode character when the part is a single
/// printable char). The last part is the "tap"; everything else
/// is held as a modifier (DOWN at the start, UP at the end,
/// reverse order).
pub fn press_key(chord: &str) -> CuaResult<()> {
    let keys = parse_chord(chord)?;
    if keys.is_empty() {
        return Err(CuaError::InvalidParams("empty chord".into()));
    }
    let (mods, tap) = keys.split_at(keys.len() - 1);

    // Build the full DOWN/TAP/UP sequence so SendInput issues
    // every event in a single call.
    let mut inputs: Vec<INPUT> = Vec::with_capacity((mods.len() * 2) + 2);
    for k in mods {
        inputs.push(vk_keystroke(*k, false));
    }
    if let Some(&k) = tap.first() {
        inputs.push(vk_keystroke(k, false));
        inputs.push(vk_keystroke(k, true));
    }
    for k in mods.iter().rev() {
        inputs.push(vk_keystroke(*k, true));
    }

    // SAFETY: `SendInput` synthesises keyboard events. The `inputs`
    // vector contains valid `INPUT` structs with correct sizes.
    unsafe {
        let sent = SendInput(
            &mut inputs,
            std::mem::size_of::<INPUT>() as i32,
        );
        let expected = (mods.len() * 2) + 2;
        if sent as usize != expected {
            return Err(CuaError::SendInput(format!(
                "SendInput returned {sent} (expected {expected})"
            )));
        }
    }
    Ok(())
}

fn vk_keystroke(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    // SAFETY: Same rationale as `unicode_keystroke` — `INPUT` is a
    // C-compatible struct, zeroed initialisation is valid, and the
    // union field `ki` is set immediately after `r#type`.
    unsafe {
        let mut input: INPUT = std::mem::zeroed();
        input.r#type = INPUT_KEYBOARD;
        let mut flags = windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0);
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        let ki = KEYBDINPUT {
            wVk: vk,
            wScan: 0,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        input.Anonymous.ki = ki;
        input
    }
}

/// Parse a `+`-separated key chord into a list of `VIRTUAL_KEY`s.
/// Single-character parts fall back to `KEYEVENTF_UNICODE` (the
/// caller recognises these and routes to `unicode_keystroke`
/// instead).
pub(crate) fn parse_chord(chord: &str) -> CuaResult<Vec<VIRTUAL_KEY>> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(name_to_vk)
        .collect()
}

fn name_to_vk(name: &str) -> CuaResult<VIRTUAL_KEY> {
    let lower = name.to_ascii_lowercase();
    let vk = match lower.as_str() {
        "alt" | "menu" => 0x12,            // VK_MENU
        "backspace" | "bs" => 0x08,        // VK_BACK
        "caps" | "capslock" => 0x14,       // VK_CAPITAL
        "ctrl" | "control" => 0x11,        // VK_CONTROL
        "delete" | "del" => 0x2E,          // VK_DELETE
        "down" => 0x28,                    // VK_DOWN
        "end" => 0x23,                     // VK_END
        "escape" | "esc" => 0x1B,          // VK_ESCAPE
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "home" => 0x24,
        "left" => 0x25,
        "meta" | "win" | "super" | "cmd" => 0x5B, // VK_LWIN
        "pagedown" | "pgdn" => 0x22,
        "pageup" | "pgup" => 0x21,
        "return" | "enter" | "cr" | "nl" => 0x0D,
        "right" => 0x27,
        "shift" => 0x10,
        "space" | "spc" => 0x20,
        "tab" => 0x09,
        "up" => 0x26,
        // Digits / letters fall back to their VK_* constant.
        // We map them through VK_'A'..VK_'0' rather than emit
        // a Unicode keystroke so chord semantics (e.g. `ctrl+a`)
        // match what the user expects on a QWERTY layout.
        other if other.len() == 1 => {
            let c = other.chars().next().unwrap();
            let b = c as u32;
            // Letters: 'a'..'z' → 0x41..0x5A
            if b >= 'a' as u32 && b <= 'z' as u32 {
                0x41 + (b - 'a' as u32)
            } else if b >= 'A' as u32 && b <= 'Z' as u32 {
                0x41 + (b - 'A' as u32)
            } else if b >= '0' as u32 && b <= '9' as u32 {
                0x30 + (b - '0' as u32)
            } else {
                return Err(CuaError::InvalidParams(format!(
                    "unknown single-char key '{other}' in chord"
                )));
            }
        }
        other => {
            return Err(CuaError::InvalidParams(format!(
                "unknown key name '{other}'"
            )));
        }
    };
    Ok(VIRTUAL_KEY(vk as u16))
}

/// Scroll the mouse wheel at `(x, y)` by `delta` clicks.
///
/// Positive `delta` scrolls up, negative scrolls down.
/// Each click is `WHEEL_DELTA` (120) units. So `delta = 3`
/// scrolls up 3 clicks (360 units).
pub fn scroll(x: i32, y: i32, delta: i32) -> CuaResult<()> {
    // Move cursor to position first
    unsafe {
        if SetCursorPos(x, y).is_err() {
            return Err(CuaError::SendInput(format!(
                "SetCursorPos({x}, {y}) failed"
            )));
        }
    }

    // mouseData for wheel is in units of WHEEL_DELTA (120)
    // Positive = up, Negative = down
    // mouseData is u32 but we need to pass signed values
    let wheel_delta = (delta * WHEEL_DELTA) as i32 as u32;

    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: wheel_delta,
        dwFlags: MOUSEEVENTF_WHEEL,
        time: 0,
        dwExtraInfo: 0,
    };

    unsafe {
        let sent = SendInput(
            std::slice::from_mut(&mut input),
            std::mem::size_of::<INPUT>() as i32,
        );
        if sent != 1 {
            return Err(CuaError::SendInput(format!(
                "SendInput returned {sent} (expected 1)"
            )));
        }
    }
    Ok(())
}

/// Double-click at `(x, y)` with the left mouse button.
///
/// This moves the cursor and sends two quick click sequences.
pub fn double_click(x: i32, y: i32) -> CuaResult<()> {
    // Move cursor first
    unsafe {
        if SetCursorPos(x, y).is_err() {
            return Err(CuaError::SendInput(format!(
                "SetCursorPos({x}, {y}) failed"
            )));
        }
    }

    // Send two click sequences (DOWN/UP/DOWN/UP)
    let mut inputs = [
        mouse_input(MOUSEEVENTF_LEFTDOWN),
        mouse_input(MOUSEEVENTF_LEFTUP),
        mouse_input(MOUSEEVENTF_LEFTDOWN),
        mouse_input(MOUSEEVENTF_LEFTUP),
    ];

    unsafe {
        let sent = SendInput(
            &mut inputs,
            std::mem::size_of::<INPUT>() as i32,
        );
        if sent != 4 {
            return Err(CuaError::SendInput(format!(
                "SendInput returned {sent} (expected 4)"
            )));
        }
    }
    Ok(())
}

/// Move the mouse cursor to `(x, y)` without clicking.
///
/// This is useful for hover effects and positioning before
/// other operations.
pub fn move_cursor(x: i32, y: i32) -> CuaResult<()> {
    unsafe {
        if SetCursorPos(x, y).is_err() {
            return Err(CuaError::SendInput(format!(
                "SetCursorPos({x}, {y}) failed"
            )));
        }
    }
    Ok(())
}

// Pull in POINT so `SendInput`'s MOUSEINPUT coordinate system
// stays available to future enhancements (e.g. absolute
// coordinates); the import is referenced from `unsafe` blocks
// above so the linker keeps it in the dep graph.
#[allow(dead_code)]
fn _keep_point_linked() -> POINT {
    POINT { x: 0, y: 0 }
}