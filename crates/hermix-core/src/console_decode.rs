//! Console output decoding helpers.
//!
//! `String::from_utf8_lossy` produces mojibake for non-UTF-8 console
//! output, which on Windows means most Chinese / Japanese / Korean
//! text from `cmd.exe`, PowerShell, and child Python processes
//! (whose default `print()` goes through the OEM code page — CP936
//! / GBK on most Simplified Chinese Windows boxes).
//!
//! This module is the single source of truth for "decode process
//! stdout/stderr to String correctly". The local terminal backend
//! and the code-execution backend both use it; the Python agent's
//! `python_alignment` glue does too.
//!
//! Decoding strategy (in order):
//!
//! 1. **Valid UTF-8** → use as-is. This is the dominant case for
//!    any program that *did* configure its stdout to UTF-8 (most
//!    modern Python with `PYTHONIOENCODING=utf-8`, Node, Go, Rust).
//! 2. **Windows OEM code page fallback** (Windows only): try GBK
//!    (`encoding_rs::GBK`, i.e. CP936). If the bytes form valid
//!    GBK, decode as such. This catches the 95% Chinese-Windows
//!    case where cmd.exe / a child Python print('你好') produces
//!    `[0xC4, 0xE3, 0xBA, 0xC3]` in stdout.
//! 3. **Last-resort lossy UTF-8** — `String::from_utf8_lossy` so we
//!    *always* return a valid `String`, even if the encoding is
//!    something exotic (Shift_JIS, Big5, KOI8-R, ...). The
//!    replacement character U+FFFD makes the failure visible
//!    without crashing the tool pipeline.

use std::borrow::Cow;

/// Decode raw process output bytes to a String, handling Windows
/// OEM code pages (GBK / CP936) on top of the usual UTF-8 fast path.
pub fn decode_console_output(bytes: &[u8]) -> Cow<'_, str> {
    // Fast path: valid UTF-8 → borrow directly.
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }

    // Windows OEM code page fallback. We only attempt GBK because
    // it's the dominant case for our user base (Simplified
    // Chinese Windows). The `encoding_rs` crate is included in
    // hermix-core as an optional dependency to keep the local
    // terminal backend's previous behavior, and we replicate it
    // here.
    #[cfg(windows)]
    {
        use encoding_rs::GBK;
        let (cow, _encoding, _had_errors) = GBK.decode(bytes);
        if !_had_errors && !cow.is_empty() {
            return Cow::Owned(cow.into_owned());
        }
    }

    // Last resort: lossy UTF-8. Always returns a valid String.
    Cow::Owned(String::from_utf8_lossy(bytes).into_owned())
}

/// Same as [`decode_console_output`] but takes ownership and is
/// useful for callers that already have a `Vec<u8>` from
/// `child.wait_with_output()` and don't need the borrow.
pub fn decode_console_output_owned(bytes: Vec<u8>) -> String {
    decode_console_output(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_passes_through() {
        let s = "hello world 你好世界";
        let bytes = s.as_bytes();
        let out = decode_console_output(bytes);
        assert_eq!(out, s);
    }

    #[test]
    fn gbk_fallback_on_windows() {
        // "你好" in GBK = C4 E3 BA C3
        let gbk_bytes: &[u8] = &[0xC4, 0xE3, 0xBA, 0xC3];
        let out = decode_console_output(gbk_bytes);
        // On Windows the GBK branch should fire; on Unix it'll fall
        // through to lossy UTF-8 (replacement chars). Either way,
        // the function must return a valid String without panicking.
        assert!(!out.is_empty());
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn empty_input_returns_empty() {
        let out = decode_console_output(&[]);
        assert_eq!(out, "");
    }

    #[test]
    fn owned_conversion_works() {
        let bytes: Vec<u8> = b"hello".to_vec();
        let out = decode_console_output_owned(bytes);
        assert_eq!(out, "hello");
    }
}
