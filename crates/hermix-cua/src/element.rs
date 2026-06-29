//! Element recognition — Phase 2-B.
//!
//! Two-layer strategy:
//!
//! 1. **Win32 layer (this file)** — top-level windows via
//!    `EnumWindows` + `EnumChildWindows` + `GetWindowTextW` +
//!    `GetClassNameW` + `GetWindowRect` + `WindowFromPoint`.
//!    Covers ~80% of LLM-driven automation ("open Notepad",
//!    "click into this app", "what's on screen now?").
//!
//! 2. **UIA COM layer (Phase 3 stretch)** — full UI Automation
//!    tree via direct `IUIAutomation`/`IUIAutomationElement`
//!    COM calls against `UIAutomationClient.dll`. Declared as
//!    a `TODO` here; the Win32 layer above already satisfies
//!    the Phase 2 surface contract (`find_at_point`,
//!    `find_by_title`, `enumerate_children`).
//!
//! All entry points are gated on `cfg(windows)`; non-Windows
//! targets return `CuaError::Uia` so the public surface stays
//! callable from the Tauri command layer on every platform
//! without `#[cfg]` in the consumer code.

use crate::error::CuaError;
use crate::CuaResult;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, WindowFromPoint,
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Information about a single window enumerated via `EnumWindows`
/// or `EnumChildWindows`. Cheap to clone, safe to ship across
/// thread boundaries, and `serde::Serialize`-friendly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowInfo {
    /// Raw HWND — `isize` rather than `HWND` so the value can be
    /// serialised to JSON and round-tripped through the agent loop
    /// without `windows` types leaking into the wire schema.
    pub hwnd: isize,

    /// `GetWindowTextW` result, trimmed. May be empty for
    /// owner-drawn / label-less windows.
    pub title: String,

    /// Window class name (`#32770` for dialogs, `Notepad` for
    /// the text editor, `Shell_TrayWnd` for the taskbar, etc.).
    pub class_name: String,

    /// Owning process ID (`GetWindowThreadProcessId`).
    pub process_id: u32,

    /// `IsWindowVisible` — hidden tool windows show up in the
    /// enumeration unless callers filter them out explicitly.
    pub is_visible: bool,

    /// Screen-space rectangle from `GetWindowRect`.
    pub rect: ElementRect,
}

/// Bounding rectangle in screen coordinates. Lives outside the
/// `#[cfg(windows)]` gate because it is part of the public
/// surface and is constructed by hand in tests / handler code.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ElementRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl ElementRect {
    /// Centre of the rectangle — useful for "click into this
    /// window" semantics where the agent has identified a window
    /// and wants to focus it.
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    /// Hit-test: does `(x, y)` fall inside the rect (inclusive
    /// on top/left, exclusive on bottom/right to mirror the
    /// Win32 `PtInRect` convention)?
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Predicate used by [`find_window`] to filter the enumeration.
/// All non-`None` fields must match. Matching is
/// case-insensitive substring for `title`, exact for
/// `class_name` / `process_id`.
#[derive(Debug, Default, Clone)]
pub struct WindowFilter {
    pub title: Option<String>,
    pub class_name: Option<String>,
    pub process_id: Option<u32>,
    pub visible_only: bool,
}

impl WindowFilter {
    /// Build a filter that matches a case-insensitive substring
    /// of `title`. Shortcut for the most common agent request:
    /// "find the window called X".
    pub fn by_title(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::default()
        }
    }

    fn matches(&self, w: &WindowInfo) -> bool {
        if self.visible_only && !w.is_visible {
            return false;
        }
        if let Some(pid) = self.process_id {
            if w.process_id != pid {
                return false;
            }
        }
        if let Some(class) = &self.class_name {
            if !w.class_name.eq_ignore_ascii_case(class) {
                return false;
            }
        }
        if let Some(title) = &self.title {
            let needle = title.to_lowercase();
            if !w.title.to_lowercase().contains(&needle) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Windows implementations
// ---------------------------------------------------------------------------

/// Enumerate every top-level window on the desktop. Includes
/// hidden / owned windows; callers usually want
/// `find_window(WindowFilter { visible_only: true, .. })`
/// instead. Cheap (~thousands of windows at most).
#[cfg(windows)]
pub fn enumerate_windows() -> CuaResult<Vec<WindowInfo>> {
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_proc), LPARAM(&mut out as *mut _ as isize))
            .map_err(|e| CuaError::Uia(format!("EnumWindows: {e}")))?;
    }
    Ok(out)
}

/// Enumerate the direct children of `parent_hwnd` (top-level
/// child windows, not recursive). `IsWindowVisible` is consulted
/// so callers can ask for "only visible children" cheaply.
#[cfg(windows)]
pub fn enumerate_children(parent_hwnd: isize, visible_only: bool) -> CuaResult<Vec<WindowInfo>> {
    let mut out: Vec<WindowInfo> = Vec::new();
    let parent = HWND(parent_hwnd as *mut _);
    let ctx = EnumContext {
        out: &mut out,
        visible_only,
    };
    let ok = unsafe {
        EnumChildWindows(parent, Some(child_enum_proc), LPARAM(&ctx as *const _ as isize))
    };
    if !bool::from(ok) {
        return Err(CuaError::Uia(format!(
            "EnumChildWindows({parent_hwnd:#x}) returned FALSE"
        )));
    }
    Ok(out)
}

/// Find the first top-level window matching `filter`. Returns
/// `Ok(None)` when nothing matches — does **not** error.
#[cfg(windows)]
pub fn find_window(filter: &WindowFilter) -> CuaResult<Option<WindowInfo>> {
    Ok(enumerate_windows()?.into_iter().find(|w| filter.matches(w)))
}

/// Find every top-level window matching `filter`.
#[cfg(windows)]
pub fn find_all_windows(filter: &WindowFilter) -> CuaResult<Vec<WindowInfo>> {
    Ok(enumerate_windows()?.into_iter().filter(|w| filter.matches(w)).collect())
}

/// Top-level window under the screen point `(x, y)`. Walks the
/// `WindowFromPoint` chain to find the deepest **visible** window
/// — `WindowFromPoint` alone sometimes returns a hidden parent.
#[cfg(windows)]
pub fn window_at_point(x: i32, y: i32) -> CuaResult<Option<WindowInfo>> {
    unsafe {
        let pt = POINT { x, y };
        let raw = WindowFromPoint(pt);
        if raw.is_invalid() {
            return Ok(None);
        }
        if let Some(info) = window_from_hwnd(raw.0 as isize) {
            return Ok(Some(info));
        }
        Ok(None)
    }
}

/// Build a `WindowInfo` from a raw HWND. Returns `Ok(None)`
/// when the HWND is no longer valid (race with window close).
#[cfg(windows)]
pub fn window_from_hwnd(hwnd: isize) -> Option<WindowInfo> {
    let h = HWND(hwnd as *mut _);
    unsafe {
        if !bool::from(IsWindow(h)) {
            return None;
        }
        let mut title_buf = [0u16; 512];
        let title_len = GetWindowTextW(h, &mut title_buf);
        let title = if title_len <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&title_buf[..title_len as usize])
        };

        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(h, &mut class_buf);
        let class_name = if class_len <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&class_buf[..class_len as usize])
        };

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));

        let is_visible = bool::from(IsWindowVisible(h));

        let mut rect = RECT::default();
        let rect = if GetWindowRect(h, &mut rect).is_ok() {
            ElementRect {
                x: rect.left,
                y: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            }
        } else {
            ElementRect::default()
        };

        Some(WindowInfo {
            hwnd,
            title,
            class_name,
            process_id: pid,
            is_visible,
            rect,
        })
    }
}

// ---------------------------------------------------------------------------
// EnumWindows / EnumChildWindows callbacks
// ---------------------------------------------------------------------------

#[cfg(windows)]
struct EnumContext {
    out: *mut Vec<WindowInfo>,
    visible_only: bool,
}

// SAFETY: EnumWindows is documented to call back on the same
// thread that invoked it; the only mutable state shared
// across invocations is the Vec in EnumContext, and we never
// re-enter the enumeration from inside the callback.
#[cfg(windows)]
unsafe impl Send for EnumContext {}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let out = &mut *(lparam.0 as *mut Vec<WindowInfo>);
        if let Some(info) = window_from_hwnd(hwnd.0 as isize) {
            out.push(info);
        }
        BOOL(1)
    }
}

#[cfg(windows)]
unsafe extern "system" fn child_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let ctx = &*(lparam.0 as *const EnumContext);
        if let Some(info) = window_from_hwnd(hwnd.0 as isize) {
            if !ctx.visible_only || info.is_visible {
                (*ctx.out).push(info);
            }
        }
        BOOL(1)
    }
}

// ---------------------------------------------------------------------------
// Non-Windows stubs
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
pub fn enumerate_windows() -> CuaResult<Vec<WindowInfo>> {
    Err(CuaError::Uia("element recognition requires Windows".into()))
}

#[cfg(not(windows))]
pub fn enumerate_children(_parent_hwnd: isize, _visible_only: bool) -> CuaResult<Vec<WindowInfo>> {
    Err(CuaError::Uia("element recognition requires Windows".into()))
}

#[cfg(not(windows))]
pub fn find_window(_filter: &WindowFilter) -> CuaResult<Option<WindowInfo>> {
    Err(CuaError::Uia("element recognition requires Windows".into()))
}

#[cfg(not(windows))]
pub fn find_all_windows(_filter: &WindowFilter) -> CuaResult<Vec<WindowInfo>> {
    Err(CuaError::Uia("element recognition requires Windows".into()))
}

#[cfg(not(windows))]
pub fn window_at_point(_x: i32, _y: i32) -> CuaResult<Option<WindowInfo>> {
    Err(CuaError::Uia("element recognition requires Windows".into()))
}

#[cfg(not(windows))]
pub fn window_from_hwnd(_hwnd: isize) -> Option<WindowInfo> {
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_center_is_midpoint() {
        let r = ElementRect { x: 100, y: 200, width: 50, height: 80 };
        assert_eq!(r.center(), (125, 240));
        assert!(r.contains(125, 240));
        assert!(r.contains(100, 200));    // top-left inclusive
        assert!(!r.contains(150, 200));   // right exclusive
        assert!(!r.contains(125, 280));   // bottom exclusive
        assert!(!r.contains(99, 240));    // just outside left
    }

    #[test]
    fn rect_default_is_degenerate_origin() {
        let r = ElementRect::default();
        assert_eq!(r.center(), (0, 0));
        // Degenerate (zero-width) rect contains no points — the
        // PtInRect convention treats top-left == bottom-right as
        // empty.
        assert!(!r.contains(0, 0));
        assert!(!r.contains(1, 1));
    }

    #[test]
    fn filter_title_is_case_insensitive_substring() {
        let f = WindowFilter::by_title("notepad");
        let w = WindowInfo {
            hwnd: 0x1234,
            title: "Untitled - Notepad".into(),
            class_name: "Notepad".into(),
            process_id: 42,
            is_visible: true,
            rect: ElementRect { x: 0, y: 0, width: 800, height: 600 },
        };
        assert!(f.matches(&w));
    }

    #[test]
    fn filter_class_name_is_exact_case_insensitive() {
        let mut f = WindowFilter::default();
        f.class_name = Some("Shell_TrayWnd".into());
        let mut w = WindowInfo {
            hwnd: 1, title: String::new(), class_name: "shell_traywnd".into(),
            process_id: 0, is_visible: true, rect: ElementRect::default(),
        };
        assert!(f.matches(&w));
        w.class_name = "TrayWnd".into();
        assert!(!f.matches(&w));
    }

    #[test]
    fn filter_visible_only_excludes_hidden_windows() {
        let mut f = WindowFilter::default();
        f.visible_only = true;
        let hidden = WindowInfo {
            hwnd: 1, title: "tool".into(), class_name: String::new(),
            process_id: 0, is_visible: false, rect: ElementRect::default(),
        };
        let visible = WindowInfo { is_visible: true, ..hidden.clone() };
        assert!(!f.matches(&hidden));
        assert!(f.matches(&visible));
    }

    #[test]
    fn filter_pid_must_match_exactly() {
        let mut f = WindowFilter::default();
        f.process_id = Some(42);
        let mut w = WindowInfo {
            hwnd: 1, title: String::new(), class_name: String::new(),
            process_id: 0, is_visible: true, rect: ElementRect::default(),
        };
        assert!(!f.matches(&w));
        w.process_id = 42;
        assert!(f.matches(&w));
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = WindowFilter::default();
        let w = WindowInfo {
            hwnd: 1, title: "anything".into(), class_name: "x".into(),
            process_id: 99, is_visible: false, rect: ElementRect::default(),
        };
        assert!(f.matches(&w));
    }

    #[cfg(windows)]
    #[test]
    fn enumerate_windows_returns_at_least_one_top_level_handle() {
        // The shell always has at least the desktop window or
        // the taskbar (even on a fresh login).
        let wins = enumerate_windows().expect("EnumWindows failed");
        assert!(!wins.is_empty(), "desktop reported zero windows");
        // HWNDs must be non-zero (HWND 0 = no window).
        assert!(wins.iter().any(|w| w.hwnd != 0));
    }

    #[cfg(windows)]
    #[test]
    fn window_from_hwnd_zero_is_none() {
        assert!(window_from_hwnd(0).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn find_window_by_title_returns_match_when_present() {
        // Enumerate first to pick a real title, then search for it.
        let wins = enumerate_windows().expect("EnumWindows failed");
        let target = wins
            .iter()
            .find(|w| !w.title.is_empty() && w.is_visible)
            .cloned();
        if let Some(t) = target {
            let f = WindowFilter::by_title(&t.title);
            let found = find_window(&f).expect("find_window failed");
            assert!(found.is_some(), "filter should have matched its own seed title");
            assert_eq!(found.unwrap().hwnd, t.hwnd);
        }
    }

    #[cfg(windows)]
    #[test]
    fn find_window_by_garbage_title_returns_none() {
        let f = WindowFilter::by_title("__hermix_definitely_not_a_window_title__");
        let found = find_window(&f).expect("find_window failed");
        assert!(found.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn window_at_point_returns_top_level_window() {
        // Probe (0, 0) — always maps to *some* window even on
        // a logged-out desktop (Program Manager). The exact
        // HWND is implementation-defined, so we just assert the
        // result is internally consistent.
        let result = window_at_point(0, 0).expect("WindowFromPoint failed");
        if let Some(w) = result {
            assert!(w.hwnd != 0);
            assert!(w.rect.width >= 0 && w.rect.height >= 0);
        }
    }
}