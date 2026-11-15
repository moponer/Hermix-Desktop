//! Keyboard injection for the desktop automation line.
//!
//! Two defenses live here, both about `SendInput` being a *global* API:
//!
//! - **Foreground ownership** (pitfall B). `SendInput` has no target
//!   parameter — whatever holds the foreground gets the keystrokes. If the
//!   user alt-tabbed between the agent's snapshot and the keystrokes, an
//!   unverified send types the user's password into their chat window. So
//!   [`ensure_foreground`] refuses to send unless the target really is
//!   frontmost.
//! - **UIPI** (pitfall A). Input aimed at an elevated window is dropped
//!   silently by user-interface privilege isolation; `SendInput` reports
//!   this as a *short insert count*, which [`inject`] turns into an error
//!   rather than a fake success.

use hermix_core::ToolError;
use std::time::Duration;
use windows::Win32::Foundation::{GetLastError, HWND};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, VIRTUAL_KEY,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

/// `SendInput` refuses absurd batch sizes; chunking also keeps the INPUT
/// array off the 1 MB default worker-stack budget of large text pastes.
const BATCH: usize = 64;

/// Modifier bits carried by a [`Stroke`] and applied to the key that
/// follows it.
pub const MOD_CTRL: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_WIN: u8 = 8;

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;

/// What a single logical keystroke resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A virtual-key code: special keys and function keys.
    V(u16),
    /// A literal character, injected through `KEYEVENTF_UNICODE`.
    Ch(char),
}

/// One logical keystroke: `mods` held down while `key` goes down+up.
///
/// Plain text is always [`Key::Ch`] rather than a virtual-key code. A VK
/// for `;` or `/` is *layout dependent* — it types whatever the active
/// keyboard layout maps that scancode to, which silently corrupts
/// non-US layouts. The unicode path takes the character as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke {
    pub mods: u8,
    pub key: Key,
}

enum Token {
    Mod(u8),
    Key(u16),
}

fn lookup(token: &str) -> Option<Token> {
    match token {
        "CTRL" | "CONTROL" => Some(Token::Mod(MOD_CTRL)),
        "ALT" => Some(Token::Mod(MOD_ALT)),
        "SHIFT" => Some(Token::Mod(MOD_SHIFT)),
        "WIN" | "WINDOWS" => Some(Token::Mod(MOD_WIN)),

        "ENTER" | "RETURN" => Some(Token::Key(0x0D)),
        "TAB" => Some(Token::Key(0x09)),
        "ESC" | "ESCAPE" => Some(Token::Key(0x1B)),
        "SPACE" => Some(Token::Key(0x20)),
        "BACKSPACE" | "BKSP" => Some(Token::Key(0x08)),
        "DEL" | "DELETE" => Some(Token::Key(0x2E)),
        "INS" | "INSERT" => Some(Token::Key(0x2D)),
        "HOME" => Some(Token::Key(0x24)),
        "END" => Some(Token::Key(0x23)),
        "PGUP" | "PAGEUP" => Some(Token::Key(0x21)),
        "PGDN" | "PAGEDOWN" => Some(Token::Key(0x22)),
        "UP" => Some(Token::Key(0x26)),
        "DOWN" => Some(Token::Key(0x28)),
        "LEFT" => Some(Token::Key(0x25)),
        "RIGHT" => Some(Token::Key(0x27)),
        _ => {
            // F1..F12 are a contiguous VK block, so one range check beats
            // twelve match arms.
            let n: u16 = token.strip_prefix('F')?.parse().ok()?;
            (1..=12).contains(&n).then_some(Token::Key(0x6F + n))
        }
    }
}

/// Parse the `{...}` key grammar into keystrokes.
///
/// Accepted: `{ENTER}`, `{TAB}`, `{ESC}`, `{F4}`, the `CTRL` / `ALT` /
/// `SHIFT` / `WIN` modifiers, and any other character as literal text.
/// A modifier holds for the next keystroke only, so `{CTRL}a` is
/// Ctrl+A and `{CTRL}{ALT}del` is Ctrl+Alt+Del. An unterminated or
/// unknown `{...}` token is an error rather than literal text — typing
/// `{CTRL}` into a text field because the model misspelled the token is
/// worse than failing the call.
pub fn parse_keys(spec: &str) -> Result<Vec<Stroke>, ToolError> {
    let mut strokes = Vec::new();
    let mut mods = 0u8;
    let mut literal = String::new();
    let mut rest = spec;

    // A modifier is consumed by the *first* keystroke it reaches:
    // `{CTRL}ab` is Ctrl+A then `b`, not Ctrl+A then Ctrl+B. Letting it ride
    // along turns "select all, then type" into "select all, then overwrite with
    // a selected B" — the classic destructive surprise.
    let flush = |literal: &mut String, mods: &mut u8, out: &mut Vec<Stroke>| {
        for (i, c) in literal.chars().enumerate() {
            out.push(Stroke {
                mods: if i == 0 { *mods } else { 0 },
                key: Key::Ch(c),
            });
        }
        literal.clear();
        *mods = 0;
    };

    while let Some(idx) = rest.find('{') {
        literal.push_str(&rest[..idx]);
        rest = &rest[idx..];
        let close = rest
            .find('}')
            .ok_or_else(|| ToolError::InvalidParams(format!("按键标记缺少右花括号：{rest}")))?;
        let token = &rest[1..close];
        rest = &rest[close + 1..];

        let upper = token.to_uppercase();
        let Some(parsed) = lookup(&upper) else {
            return Err(ToolError::InvalidParams(format!(
                "未知的按键标记 {{{upper}}}；可用：ENTER TAB ESC SPACE BACKSPACE DEL INS HOME END PGUP PGDN UP DOWN LEFT RIGHT F1-F12 CTRL ALT SHIFT WIN"
            )));
        };
        match parsed {
            Token::Mod(bit) => {
                flush(&mut literal, &mut mods, &mut strokes);
                mods |= bit;
            }
            Token::Key(vk) => {
                // Read the pending modifier before `flush` consumes it: the
                // named key is the one the modifier belongs to.
                let with_mods = mods;
                flush(&mut literal, &mut mods, &mut strokes);
                strokes.push(Stroke { mods: with_mods, key: Key::V(vk) });
            }
        }
    }
    literal.push_str(rest);
    flush(&mut literal, &mut mods, &mut strokes);
    Ok(strokes)
}

/// Arrows, navigation and the keypad-adjacent keys live on extended
/// scancodes; without the flag Windows delivers the *numpad* variant and
/// the cursor stays put.
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x2D | 0x5B | 0x5C | 0x6F..=0x7B)
}

fn vk_down(vk: u16) -> INPUT {
    let flags = if is_extended(vk) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn vk_up(vk: u16) -> INPUT {
    let mut flags = KEYEVENTF_KEYUP;
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// One UTF-16 unit of a character, down and up. Non-BMP characters are
/// sent unit by unit because `KEYEVENTF_UNICODE` carries exactly one
/// `WCHAR` per record.
fn unicode_pair(unit: u16) -> [INPUT; 2] {
    let make = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    [make(false), make(true)]
}

fn mod_vks(mods: u8) -> Vec<u16> {
    let mut vks = Vec::with_capacity(4);
    if mods & MOD_CTRL != 0 {
        vks.push(VK_CONTROL);
    }
    if mods & MOD_ALT != 0 {
        vks.push(VK_MENU);
    }
    if mods & MOD_SHIFT != 0 {
        vks.push(VK_SHIFT);
    }
    if mods & MOD_WIN != 0 {
        vks.push(VK_LWIN);
    }
    vks
}

/// Expand strokes into the exact INPUT sequence to send.
pub fn to_inputs(strokes: &[Stroke]) -> Vec<INPUT> {
    let mut out = Vec::with_capacity(strokes.len() * 6);
    for stroke in strokes {
        let held = mod_vks(stroke.mods);
        out.extend(held.iter().copied().map(vk_down));
        match stroke.key {
            Key::V(vk) => {
                out.push(vk_down(vk));
                out.push(vk_up(vk));
            }
            Key::Ch(c) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf).iter().copied() {
                    out.extend(unicode_pair(unit));
                }
            }
        }
        out.extend(held.iter().rev().copied().map(vk_up));
    }
    out
}

/// Hand the sequence to Windows and insist that all of it landed.
///
/// A short insert count is how UIPI reports "the target window runs
/// elevated and you do not" — the keystrokes are gone and no error code
/// ever reaches us otherwise.
fn inject(inputs: &[INPUT]) -> Result<(), ToolError> {
    let size = std::mem::size_of::<INPUT>() as i32;
    for chunk in inputs.chunks(BATCH) {
        let sent = unsafe { SendInput(chunk, size) } as usize;
        if sent != chunk.len() {
            return Err(ToolError::PermissionDenied(format!(
                "SendInput 只写入了 {sent}/{} 个按键：输入被 UIPI 拦截，目标窗口以管理员权限运行而 hermix 未提权",
                chunk.len()
            )));
        }
    }
    Ok(())
}

/// Refuse to type anywhere except the intended target.
///
/// `target = None` means "whatever the user has focused", which is the
/// explicit contract of `win_send_keys` without a `window` argument.
pub fn ensure_foreground(target: Option<HWND>) -> Result<(), ToolError> {
    let Some(target) = target else { return Ok(()) };
    if unsafe { GetForegroundWindow() } == target {
        return Ok(());
    }
    let switched = unsafe { SetForegroundWindow(target) }.0 != 0;
    // Captured here: any later call (including the sleep below) may clobber
    // the thread's last-error slot.
    let err = unsafe { GetLastError() }.0;
    if !switched && err == 5 {
        return Err(ToolError::PermissionDenied(
            "SetForegroundWindow 被拒绝（ERROR_ACCESS_DENIED）：目标窗口以管理员权限运行，hermix 未提权"
                .into(),
        ));
    }
    // Activation is not instantaneous — the foreground lock releases with
    // the message pump, so re-check after a beat instead of assuming.
    // Blocking sleep is safe here: this only ever runs on the sync UIA
    // worker thread, never on an async executor.
    std::thread::sleep(Duration::from_millis(60));
    if unsafe { GetForegroundWindow() } == target {
        return Ok(());
    }
    Err(ToolError::PermissionDenied(
        "无法把目标窗口切到前台，SendInput 会打进用户当前正在用的窗口，已中止".into(),
    ))
}

/// Type literal text (no `{...}` parsing) into `target`.
pub fn send_text(text: &str, target: Option<HWND>) -> Result<(), ToolError> {
    ensure_foreground(target)?;
    let mut inputs = Vec::with_capacity(text.chars().count() * 2);
    let mut buf = [0u16; 2];
    for c in text.chars() {
        for unit in c.encode_utf16(&mut buf).iter().copied() {
            inputs.extend(unicode_pair(unit));
        }
    }
    inject(&inputs)
}

/// Send a parsed key sequence into `target`.
pub fn press_keys(strokes: &[Stroke], target: Option<HWND>) -> Result<(), ToolError> {
    ensure_foreground(target)?;
    inject(&to_inputs(strokes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vks(spec: &str) -> Vec<Key> {
        parse_keys(spec).unwrap().into_iter().map(|s| s.key).collect()
    }

    #[test]
    fn named_tokens_map_to_vk() {
        assert_eq!(vks("{ENTER}{TAB}{ESC}"), vec![Key::V(0x0D), Key::V(0x09), Key::V(0x1B)]);
        assert_eq!(vks("{F1}{F12}"), vec![Key::V(0x70), Key::V(0x7B)]);
        assert_eq!(vks("{DOWN}{PGDN}"), vec![Key::V(0x28), Key::V(0x22)]);
    }

    #[test]
    fn unknown_token_is_an_error_not_literal_text() {
        let err = parse_keys("hello {NOPE} world").unwrap_err();
        assert!(err.to_string().contains("NOPE"), "{err}");
    }

    #[test]
    fn unterminated_token_is_an_error() {
        assert!(parse_keys("{CTRL").is_err());
        assert!(parse_keys("a}b").is_ok());
    }

    #[test]
    fn plain_text_becomes_unicode_strokes() {
        assert_eq!(vks("hi!"), vec![Key::Ch('h'), Key::Ch('i'), Key::Ch('!')]);
        // A literal brace must be escaped by the caller, not guessed at.
        assert!(parse_keys("{").is_err());
    }

    #[test]
    fn modifier_applies_to_the_next_key_only() {
        let strokes = parse_keys("{CTRL}a").unwrap();
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].mods, MOD_CTRL);
        assert_eq!(strokes[0].key, Key::Ch('a'));

        let strokes = parse_keys("{ALT}{F4}").unwrap();
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].mods, MOD_ALT);
        assert_eq!(strokes[0].key, Key::V(0x73));

        // Ctrl+A then a bare B: one modifier, one keystroke. Without the
        // consume-on-use rule this becomes "select all, then overwrite it
        // with a selected B" — the classic destructive surprise.
        let strokes = parse_keys("{CTRL}ab").unwrap();
        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes.iter().map(|s| s.mods).collect::<Vec<_>>(), vec![MOD_CTRL, 0]);

        // An explicit second modifier is honoured, not swallowed.
        let strokes = parse_keys("{CTRL}a{CTRL}b").unwrap();
        assert_eq!(strokes.iter().map(|s| s.mods).collect::<Vec<_>>(), vec![MOD_CTRL, MOD_CTRL]);
    }

    #[test]
    fn trailing_modifier_emits_no_keystroke() {
        // Nothing to release: mods only materialize when a key consumes them.
        assert!(parse_keys("{CTRL}").unwrap().is_empty());
    }

    #[test]
    fn mixed_grammar_keeps_order() {
        let strokes = parse_keys("{CTRL}c{ENTER}").unwrap();
        assert_eq!(
            strokes.iter().map(|s| s.key.clone()).collect::<Vec<_>>(),
            vec![Key::Ch('c'), Key::V(0x0D)]
        );
    }

    #[test]
    fn non_bmp_char_splits_into_utf16_units() {
        let inputs = to_inputs(&[Stroke { mods: 0, key: Key::Ch('\u{1F600}') }]);
        assert_eq!(inputs.len(), 4, "surrogate pair needs two down/up records");
    }

    #[test]
    fn stroke_emits_mod_down_around_the_key() {
        let inputs = to_inputs(&[Stroke { mods: MOD_CTRL, key: Key::V(0x41) }]);
        // ctrl down, a down, a up, ctrl up
        assert_eq!(inputs.len(), 4);
        // Reading a union field is unsafe by definition; every INPUT here was
        // built as a keyboard record by `to_inputs`, so the read is sound.
        let vk = |i: &INPUT| unsafe { i.Anonymous.ki.wVk.0 };
        let flags = |i: &INPUT| unsafe { i.Anonymous.ki.dwFlags.0 };
        assert_eq!(vk(&inputs[0]), VK_CONTROL);
        assert_eq!(vk(&inputs[1]), 0x41);
        assert_eq!(vk(&inputs[3]), VK_CONTROL);
        assert_eq!(flags(&inputs[0]) & KEYEVENTF_KEYUP.0, 0);
        assert_ne!(flags(&inputs[3]) & KEYEVENTF_KEYUP.0, 0);
    }

    #[test]
    fn arrows_carry_the_extended_scancode_flag() {
        let inputs = to_inputs(&[Stroke { mods: 0, key: Key::V(0x25) }]);
        assert_ne!(
            unsafe { inputs[0].Anonymous.ki.dwFlags.0 } & KEYEVENTF_EXTENDEDKEY.0,
            0
        );
    }
}
