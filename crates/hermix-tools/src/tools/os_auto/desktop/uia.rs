//! UI Automation client for the desktop automation line.
//!
//! Everything that touches COM lives behind [`Session`], which is owned by
//! the one MTA worker thread (see [`super::worker`]). The pure logic —
//! reference bookkeeping, line rendering, clamping, hwnd parsing — is
//! deliberately separated from the COM calls so it can be unit-tested on a
//! headless machine with no desktop session.
//!
//! Identity: UIA elements have no address the caller can hold, so a
//! snapshot hands out short `e12`-style handles and [`RefTable`] records
//! which snapshot generation produced them. Acting on a handle from an
//! older generation would hit whatever element now occupies that slot, so
//! stale handles are rejected outright.

use hermix_core::ToolError;
use std::collections::HashMap;
use windows::core::{BSTR, IUnknown};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern, IUIAutomationTextPattern,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, UIA_ButtonControlTypeId,
    UIA_CalendarControlTypeId, UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId,
    UIA_CustomControlTypeId, UIA_DataGridControlTypeId, UIA_DataItemControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_GroupControlTypeId,
    UIA_HeaderControlTypeId, UIA_HeaderItemControlTypeId, UIA_HyperlinkControlTypeId,
    UIA_ImageControlTypeId, UIA_InvokePatternId, UIA_ListControlTypeId, UIA_ListItemControlTypeId,
    UIA_MenuBarControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId,
    UIA_PaneControlTypeId, UIA_ProgressBarControlTypeId, UIA_RadioButtonControlTypeId,
    UIA_ScrollBarControlTypeId, UIA_SemanticZoomControlTypeId, UIA_SeparatorControlTypeId,
    UIA_SliderControlTypeId, UIA_SpinnerControlTypeId, UIA_SplitButtonControlTypeId,
    UIA_StatusBarControlTypeId, UIA_TabControlTypeId, UIA_TabItemControlTypeId,
    UIA_TableControlTypeId, UIA_TextControlTypeId, UIA_TextPatternId, UIA_ThumbControlTypeId,
    UIA_TitleBarControlTypeId, UIA_ToolBarControlTypeId, UIA_ToolTipControlTypeId,
    UIA_TreeControlTypeId, UIA_TreeItemControlTypeId, UIA_ValuePatternId, UIA_WindowControlTypeId,
    CUIAutomation8, UIA_CONTROLTYPE_ID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};

/// Longest window title echoed back. Titles are arbitrary user data — an
/// editor tab can hold a 4000-character path, and none of it is needed to
/// pick a window.
const TITLE_CAP: usize = 120;
/// Longest element name inside a snapshot line, for the same reason.
const NAME_CAP: usize = 80;
/// Hard cap on `win_list_windows` rows: the tool exists so the agent picks a
/// window, not so it reads a task manager.
const MAX_WINDOWS: usize = 50;

const STALE_REF: &str = "快照已过期，请重新调用 win_snapshot";

/// Truncate on a **character** boundary lives in the parent module — both OS
/// automation lines return Chinese UI text and share the rule.
use super::super::truncate_chars;

/// Element and window names may contain newlines, tabs or NULs. Left
/// alone they would break the one-line-per-element format the whole
/// snapshot format depends on.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The generated `UIA_*` constants are mixed-case, so matching on them trips
/// `non_upper_case_globals`. They are Win32's names, not ours to rename.
#[allow(non_upper_case_globals)]
pub fn control_type_name(id: UIA_CONTROLTYPE_ID) -> &'static str {
    match id {
        UIA_ButtonControlTypeId => "Button",
        UIA_CalendarControlTypeId => "Calendar",
        UIA_CheckBoxControlTypeId => "CheckBox",
        UIA_ComboBoxControlTypeId => "ComboBox",
        UIA_DataGridControlTypeId => "DataGrid",
        UIA_DataItemControlTypeId => "DataItem",
        UIA_DocumentControlTypeId => "Document",
        UIA_EditControlTypeId => "Edit",
        UIA_GroupControlTypeId => "Group",
        UIA_HeaderControlTypeId => "Header",
        UIA_HeaderItemControlTypeId => "HeaderItem",
        UIA_HyperlinkControlTypeId => "Hyperlink",
        UIA_ImageControlTypeId => "Image",
        UIA_ListControlTypeId => "List",
        UIA_ListItemControlTypeId => "ListItem",
        UIA_MenuBarControlTypeId => "MenuBar",
        UIA_MenuControlTypeId => "Menu",
        UIA_MenuItemControlTypeId => "MenuItem",
        UIA_PaneControlTypeId => "Pane",
        UIA_ProgressBarControlTypeId => "ProgressBar",
        UIA_RadioButtonControlTypeId => "RadioButton",
        UIA_ScrollBarControlTypeId => "ScrollBar",
        UIA_SemanticZoomControlTypeId => "SemanticZoom",
        UIA_SeparatorControlTypeId => "Separator",
        UIA_SliderControlTypeId => "Slider",
        UIA_SpinnerControlTypeId => "Spinner",
        UIA_SplitButtonControlTypeId => "SplitButton",
        UIA_StatusBarControlTypeId => "StatusBar",
        UIA_TabControlTypeId => "Tab",
        UIA_TabItemControlTypeId => "TabItem",
        UIA_TableControlTypeId => "Table",
        UIA_TextControlTypeId => "Text",
        UIA_ThumbControlTypeId => "Thumb",
        UIA_TitleBarControlTypeId => "TitleBar",
        UIA_ToolBarControlTypeId => "ToolBar",
        UIA_ToolTipControlTypeId => "ToolTip",
        UIA_TreeControlTypeId => "Tree",
        UIA_TreeItemControlTypeId => "TreeItem",
        UIA_WindowControlTypeId => "Window",
        _ => "Custom",
    }
}

/// Render one element: `ref=e12 Button "确定" [enabled,focus]`.
pub fn render_line(
    ref_id: &str,
    depth: u32,
    control_type: UIA_CONTROLTYPE_ID,
    name: &str,
    states: &[&str],
) -> String {
    format!(
        "{:indent$}ref={ref_id} {} \"{}\" [{}]",
        "",
        control_type_name(control_type),
        truncate_chars(&sanitize(name), NAME_CAP),
        states.join(","),
        indent = depth as usize * 2
    )
}

/// Accept both the `0x1a2b` token `win_list_windows` prints and a bare
/// decimal hwnd.
pub fn parse_hwnd(s: &str) -> Result<HWND, ToolError> {
    let t = s.trim();
    let raw = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => t.parse::<u64>(),
    };
    let value = raw
        .map_err(|_| ToolError::InvalidParams(format!("无法解析窗口句柄 `{s}`（需要 0x 前缀的十六进制或十进制）")))?;
    Ok(HWND(value as *mut core::ffi::c_void))
}

/// Hands out the `e12`-style handles in one snapshot.
///
/// Split from the element map so the numbering — and the reset that makes
/// the previous snapshot's handles miss — is testable without a desktop.
#[derive(Debug, Default)]
pub struct RefIds(usize);

impl RefIds {
    /// Retire the previous snapshot. Ids restart at `e1`; the caller clears
    /// the element map in the same breath, so every old handle now misses.
    pub fn reset(&mut self) {
        self.0 = 0;
    }

    pub fn next(&mut self) -> String {
        self.0 += 1;
        format!("e{}", self.0)
    }
}

/// A handle that is not in the current table.
///
/// One message for "nonsense handle" and "that snapshot is three turns
/// old": after a new snapshot the agent's overwhelmingly likely mistake is
/// the second one, and naming it tells it to re-snapshot instead of
/// retrying.
fn miss_ref() -> ToolError {
    ToolError::InvalidParams(STALE_REF.into())
}

struct WinRow {
    hwnd: HWND,
    title: String,
    class: String,
    pid: u32,
    rect: RECT,
    foreground: bool,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, param: LPARAM) -> BOOL {
    // The collector is a `&mut Vec` behind the callback's LPARAM: Win32
    // offers no way to return state, and a global would need its own lock
    // on a hot path.
    let rows = unsafe { &mut *(param.0 as *mut Vec<WinRow>) };
    if unsafe { IsWindowVisible(hwnd) }.0 == 0 {
        return BOOL(1);
    }
    let mut title_buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut title_buf) };
    if n <= 0 {
        // Unnamed windows are message-only helpers, shell surfaces and
        // zero-size ghosts — never the target of an automation call.
        return BOOL(1);
    }
    let mut class_buf = [0u16; 256];
    let cn = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32)) };
    // RECT derives no Default in windows 0.58, and GetWindowRect wants a
    // pre-allocated out-param anyway.
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    rows.push(WinRow {
        hwnd,
        title: truncate_chars(&String::from_utf16_lossy(&title_buf[..n as usize]), TITLE_CAP),
        class: String::from_utf16_lossy(&class_buf[..cn.max(0) as usize]),
        pid,
        rect,
        foreground: fg == hwnd,
    });
    BOOL(1)
}

/// The thread's UIA client plus the snapshot reference table.
pub struct Session {
    automation: IUIAutomation,
    /// Control view, not raw view: raw view includes every provider-owned
    /// fragment and routinely runs 10x deeper, which is the tree explosion
    /// the node cap exists to bound. An agent wants the controls a human
    /// sees.
    walker: IUIAutomationTreeWalker,
    refs: RefIds,
    /// The only identity a `ref` has: a strong COM reference, created and
    /// released inside this thread's apartment. Replaced wholesale per
    /// snapshot, which bounds it by `max_nodes` and makes membership the
    /// staleness test.
    elements: HashMap<String, IUIAutomationElement>,
}

impl Session {
    /// Must be called on the thread that owns the apartment.
    pub fn new() -> Result<Self, ToolError> {
        let automation: IUIAutomation = unsafe {
            CoCreateInstance(&CUIAutomation8, None::<&IUnknown>, CLSCTX_INPROC_SERVER)
        }
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("创建 UIA 客户端（CUIAutomation8）失败：{e}"))
        })?;
        let walker = unsafe { automation.ControlViewWalker() }
            .map_err(|e| ToolError::ExecutionFailed(format!("获取 UIA 树遍历器失败：{e}")))?;
        Ok(Self {
            automation,
            walker,
            refs: RefIds::default(),
            elements: HashMap::new(),
        })
    }

    /// Resolve a snapshot handle to its live element.
    ///
    /// A miss is the staleness check: the table was replaced by a newer
    /// snapshot, so this handle no longer names anything the caller may act
    /// on.
    fn resolve(&self, ref_id: &str) -> Result<IUIAutomationElement, ToolError> {
        self.elements.get(ref_id).cloned().ok_or_else(miss_ref)
    }

    /// Top-level visible windows, front-to-back, capped at [`MAX_WINDOWS`].
    pub fn list_windows(&self, filter: Option<&str>) -> Result<String, ToolError> {
        let needle = filter.map(|f| f.to_lowercase());
        let mut rows: Vec<WinRow> = Vec::new();
        unsafe {
            EnumWindows(
                Some(enum_cb),
                LPARAM(&mut rows as *mut Vec<WinRow> as isize),
            )
        }
        .map_err(|e| ToolError::ExecutionFailed(format!("枚举窗口失败：{e}")))?;

        if let Some(n) = needle.as_deref().filter(|n| !n.is_empty()) {
            rows.retain(|r| r.title.to_lowercase().contains(n));
        }
        if rows.len() > MAX_WINDOWS {
            rows.truncate(MAX_WINDOWS);
        }

        let mut out = format!(
            "共 {} 个可见窗口（最多显示 {MAX_WINDOWS}，按 Z 序从前往后）：\n",
            rows.len()
        );
        if rows.is_empty() {
            out.push_str("（没有匹配窗口——先去掉 filter 再试）");
            return Ok(out);
        }
        for r in &rows {
            out.push_str(&format!(
                "hwnd=0x{:x} pid={} class={} rect=({},{},{},{}){} \"{}\"\n",
                r.hwnd.0 as usize,
                r.pid,
                truncate_chars(&r.class, 48),
                r.rect.left,
                r.rect.top,
                r.rect.right,
                r.rect.bottom,
                if r.foreground { " [FOREGROUND]" } else { "" },
                r.title
            ));
        }
        Ok(out)
    }

    /// The element a call is aimed at, plus a human label for the header.
    fn root(&self, window: Option<HWND>) -> Result<(IUIAutomationElement, String), ToolError> {
        let hwnd = match window {
            Some(h) => h,
            None => unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() },
        };
        if hwnd.0.is_null() {
            return Err(ToolError::ExecutionFailed(
                "没有前台窗口（当前桌面可能已锁定或无交互会话）".into(),
            ));
        }
        let label = match window {
            Some(_) => format!("窗口 hwnd=0x{:x}", hwnd.0 as usize),
            None => format!("前台窗口 hwnd=0x{:x}", hwnd.0 as usize),
        };
        let el = unsafe { self.automation.ElementFromHandle(hwnd) }
            .map_err(|e| ToolError::ExecutionFailed(format!("UIA 无法附着到 {label}：{e}")))?;
        Ok((el, label))
    }

    /// Walk the control tree under `root`, one line per element.
    ///
    /// Iterative on purpose: UIA trees are 100+ levels deep in Electron
    /// apps, and a recursive walk blows the worker thread's stack long
    /// before the depth cap would stop it.
    pub fn snapshot(
        &mut self,
        window: Option<HWND>,
        max_depth: u32,
        max_nodes: u32,
    ) -> Result<String, ToolError> {
        let (root, label) = self.root(window)?;
        self.refs.reset();
        self.elements.clear();

        let mut out = format!(
            "win_snapshot · {label} · 深度上限 {max_depth} · 节点上限 {max_nodes}（ref 只在本次快照内有效）\n"
        );
        let mut stack = vec![(root, 0u32)];
        let mut emitted = 0u32;
        let mut hit_depth = false;
        let mut hit_nodes = false;

        while let Some((el, depth)) = stack.pop() {
            if emitted >= max_nodes {
                hit_nodes = true;
                break;
            }
            emitted += 1;

            // Each property read is a separate COM round trip into a
            // provider that may have died since the last one, so a failure
            // degrades to a placeholder instead of aborting the walk.
            let ct = unsafe { el.CurrentControlType() }.unwrap_or(UIA_CustomControlTypeId);
            let name = unsafe { el.CurrentName() }
                .map(|b| b.to_string())
                .unwrap_or_default();
            let mut states: Vec<&str> = Vec::new();
            if unsafe { el.CurrentIsEnabled() }.map(|b| b.0 != 0).unwrap_or(false) {
                states.push("enabled");
            }
            if unsafe { el.CurrentIsOffscreen() }
                .map(|b| b.0 != 0)
                .unwrap_or(false)
            {
                states.push("offscreen");
            }
            if unsafe { el.CurrentHasKeyboardFocus() }
                .map(|b| b.0 != 0)
                .unwrap_or(false)
            {
                states.push("focus");
            }

            let id = self.refs.next();
            self.elements.insert(id.clone(), el.clone());
            out.push_str(&render_line(&id, depth, ct, &name, &states));
            out.push('\n');

            if depth >= max_depth {
                hit_depth = true;
                continue;
            }
            let mut children: Vec<IUIAutomationElement> = Vec::new();
            let mut cursor = unsafe { self.walker.GetFirstChildElement(&el) }.ok();
            while let Some(child) = cursor {
                if emitted as usize + children.len() >= max_nodes as usize {
                    hit_nodes = true;
                    break;
                }
                let next = unsafe { self.walker.GetNextSiblingElement(&child) }.ok();
                children.push(child);
                cursor = next;
            }
            children.reverse();
            stack.extend(children.into_iter().map(|c| (c, depth + 1)));
        }

        if emitted == 0 {
            out.push_str("（该窗口没有暴露任何 UIA 元素；它是自绘界面，请改用 win_read_text 或 win_send_keys）\n");
        }
        if hit_nodes {
            out.push_str(&format!(
                "… 已达节点上限 {max_nodes}，未展开其余节点（调高 max_nodes 或缩小目标区域）\n"
            ));
        }
        if hit_depth {
            out.push_str(&format!("… 已达深度上限 {max_depth}，更深层未展开\n"));
        }
        Ok(out)
    }

    /// Press the element's Invoke pattern.
    pub fn invoke(&self, ref_id: &str) -> Result<String, ToolError> {
        let el = self.resolve(ref_id)?;
        let pattern: IUIAutomationInvokePattern = unsafe { el.GetCurrentPatternAs(UIA_InvokePatternId) }
            .map_err(|_| {
                ToolError::PermissionDenied(format!(
                    "元素 `{ref_id}` 不支持 Invoke 模式（它可能不是可点击控件，或目标窗口以管理员权限运行、调用被 UIPI 静默拦截）"
                ))
            })?;
        unsafe { pattern.Invoke() }.map_err(|e| {
            ToolError::ExecutionFailed(format!("Invoke 失败：{e}"))
        })?;
        let ct = unsafe { el.CurrentControlType() }.unwrap_or(UIA_CustomControlTypeId);
        Ok(format!(
            "已调用 {ref_id}（{}）",
            control_type_name(ct)
        ))
    }

    /// Write `value` through the element's Value pattern.
    pub fn set_value(&self, ref_id: &str, value: &str) -> Result<String, ToolError> {
        let el = self.resolve(ref_id)?;
        let pattern: IUIAutomationValuePattern = unsafe { el.GetCurrentPatternAs(UIA_ValuePatternId) }
            .map_err(|_| {
                ToolError::PermissionDenied(format!(
                    "元素 `{ref_id}` 不支持 Value 模式（只读文本框/标签不支持，或目标窗口以管理员权限运行、调用被 UIPI 静默拦截）"
                ))
            })?;
        let bstr = BSTR::from(value);
        unsafe { pattern.SetValue(&bstr) }
            .map_err(|e| ToolError::ExecutionFailed(format!("SetValue 失败：{e}")))?;
        Ok(format!("已向 {ref_id} 写入 {} 个字符", value.chars().count()))
    }

    /// Pull the window's text: the whole document range when the provider
    /// offers a Text pattern, the accessible name otherwise.
    pub fn read_text(&self, window: Option<HWND>, max_chars: u32) -> Result<String, ToolError> {
        let (el, label) = self.root(window)?;
        if unsafe { el.CurrentIsPassword() }
            .map(|b| b.0 != 0)
            .unwrap_or(false)
        {
            return Ok("[已跳过：目标是密码字段]".into());
        }
        let text = match unsafe { el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
        {
            Ok(pattern) => {
                let range = unsafe { pattern.DocumentRange() }
                    .map_err(|e| ToolError::ExecutionFailed(format!("读取文本范围失败：{e}")))?;
                // Ask the provider for the cap directly; letting it build a
                // multi-megabyte string and clipping afterwards is exactly
                // the context bloat this avoids.
                let raw = unsafe { range.GetText(max_chars as i32) }
                    .map_err(|e| ToolError::ExecutionFailed(format!("提取文本失败：{e}")))?;
                raw.to_string()
            }
            Err(_) => unsafe { el.CurrentName() }
                .map(|b| b.to_string())
                .unwrap_or_default(),
        };
        let body = truncate_chars(&sanitize(&text), max_chars as usize);
        let tail = if body.is_empty() {
            "\n（该窗口没有暴露文本）".to_string()
        } else {
            format!("\n{body}")
        };
        Ok(format!(
            "win_read_text · {label} · {} 字符{}{tail}",
            body.chars().count(),
            if body.chars().count() as u32 >= max_chars { "（已截断）" } else { "" },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BTN: UIA_CONTROLTYPE_ID = UIA_ButtonControlTypeId;

    #[test]
    fn line_format_is_stable() {
        assert_eq!(
            render_line("e12", 0, BTN, "确定", &["enabled"]),
            "ref=e12 Button \"确定\" [enabled]"
        );
        assert_eq!(
            render_line("e3", 2, UIA_EditControlTypeId, "", &[]),
            "    ref=e3 Edit \"\" []"
        );
    }

    #[test]
    fn control_chars_cannot_break_the_one_line_per_element_format() {
        let line = render_line("e1", 0, BTN, "a\nb\tc", &[]);
        assert_eq!(line, "ref=e1 Button \"a?b?c\" []");
    }

    #[test]
    fn long_names_are_capped() {
        let line = render_line("e1", 0, BTN, &"字".repeat(500), &[]);
        assert!(line.chars().count() <= NAME_CAP + 40, "{}", line.chars().count());
    }

    #[test]
    fn unknown_control_types_degrade_to_custom() {
        assert_eq!(control_type_name(UIA_CONTROLTYPE_ID(12345)), "Custom");
    }

    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        let s = "你好世界";
        assert_eq!(truncate_chars(s, 2), "你好");
        assert_eq!(truncate_chars(s, 99), s);
        assert_eq!(truncate_chars(s, 0), "");
        // Byte-slicing this would panic; the char walk cannot.
        assert_eq!(truncate_chars(s, 1).chars().count(), 1);
    }

    #[test]
    fn ids_are_reissued_from_one_after_each_snapshot() {
        let mut ids = RefIds::default();
        assert_eq!(ids.next(), "e1");
        assert_eq!(ids.next(), "e2");
        ids.reset();
        assert_eq!(ids.next(), "e1");
    }

    #[test]
    fn a_ref_that_misses_the_table_gets_the_recovery_hint() {
        // Membership *is* the staleness test: a handle from a replaced
        // snapshot is simply absent, and the agent is told how to recover.
        let msg = miss_ref().to_string();
        assert!(msg.contains(STALE_REF), "{msg}");
        assert!(msg.contains("win_snapshot"), "{msg}");
    }

    #[test]
    fn hwnd_accepts_hex_and_decimal() {
        assert_eq!(parse_hwnd("0x1a2b").unwrap().0 as usize, 0x1a2b);
        assert_eq!(parse_hwnd("6699").unwrap().0 as usize, 6699);
        assert!(parse_hwnd("not-a-hwnd").is_err());
    }
}
