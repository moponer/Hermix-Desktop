//! Win32 桌面自动化：列窗口 / 找控件 / 点击 / 填文本 / 按键。
//!
//! **为什么不是 UI Automation**：UI Automation（`hermix-tools` 的
//! `os_auto/desktop/uia.rs`）能力更强，能拿到无障碍元素树，但依赖 `windows`
//! crate 的 COM 接口层，且 hermix-tools 本身绑着 chromiumoxide / rusqlite /
//! docx-rs 等一整套依赖——viewer 是 8 个依赖的轻量 sidecar，不该背这些。
//! 这里用 `windows-sys`（已在依赖树）直接发 Win32 消息，够点按钮、填输入框、
//! 触发按键，且**零新依赖**。
//!
//! **能力边界**（写清楚，别让调用方误以为能做 UIA 的事）：
//!   - 能按窗口标题 / 控件类名 / 控件文本定位，**不能**按无障碍名称定位；
//!   - 点击走 BM_CLICK / WM_LBUTTONDOWN+UP，**不做**真实鼠标移动；
//!   - 填文本走 WM_SETTEXT（直接写控件内容），**不是**逐键敲字。
//! 想要完整 UIA 树时用 hermix agent 的 `win_snapshot` 工具。

use serde::Serialize;

/// 顶层窗口。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// 十六进制句柄（调用方后续用它做控件操作）。
    pub hwnd: String,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub visible: bool,
}

/// 子控件。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlInfo {
    pub hwnd: String,
    pub class: String,
    pub text: String,
    /// 控件 ID（对话框里用来定位按钮的稳定标识）。
    pub id: i32,
}

#[cfg(windows)]
mod imp {
    use super::{ControlInfo, WindowInfo};
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetClassNameW, GetDlgCtrlID, GetWindowThreadProcessId,
        GetWindowTextW, IsWindowVisible,
    };

    /// 窗口标题最长回显长度：标题是任意用户数据（编辑器标签里可能是几千字符
    /// 的路径），挑一个窗口用不到那么多。
    const TITLE_CAP: usize = 200;
    /// 单次列举的控件上限：工具是给人/AI「找一个控件」，不是扫控件树。
    const MAX_CONTROLS: usize = 200;
    /// 顶层窗口上限。
    const MAX_WINDOWS: usize = 50;

    pub fn window_text(hwnd: HWND) -> String {
        let mut buf = vec![0u16; 1024];
        let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 0 {
            return String::new();
        }
        let text = String::from_utf16_lossy(&buf[..len as usize]);
        text.chars().take(TITLE_CAP).collect()
    }

    pub fn class_of(hwnd: HWND) -> String {
        let mut buf = vec![0u16; 256];
        let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }

    /// 窗口/控件名里可能有换行、制表符或 NUL——不处理会破坏「一行一条」的
    /// 输出格式（也破坏 JSON 之外的日志可读性）。
    fn sanitize(text: &str) -> String {
        text.chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect()
    }

    unsafe extern "system" fn window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam as *mut Vec<WindowInfo>);
        let visible = IsWindowVisible(hwnd) != 0;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        // 自己的进程不列：调用方要操作的是别人的窗口。
        if pid != GetCurrentProcessId() {
            list.push(WindowInfo {
                hwnd: format!("0x{:X}", hwnd as usize),
                title: sanitize(&window_text(hwnd)),
                class: class_of(hwnd),
                pid,
                visible,
            });
        }
        if list.len() >= MAX_WINDOWS {
            return 0; // 停
        }
        1
    }

    unsafe extern "system" fn control_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam as *mut Vec<ControlInfo>);
        list.push(ControlInfo {
            hwnd: format!("0x{:X}", hwnd as usize),
            class: class_of(hwnd),
            text: sanitize(&window_text(hwnd)),
            id: GetDlgCtrlID(hwnd),
        });
        if list.len() >= MAX_CONTROLS {
            return 0; // 停
        }
        1
    }

    pub fn list_windows() -> Vec<WindowInfo> {
        let mut out: Vec<WindowInfo> = Vec::new();
        unsafe {
            EnumWindows(Some(window_cb), &mut out as *mut Vec<WindowInfo> as LPARAM);
        }
        out
    }

    pub fn list_controls(parent: HWND) -> Vec<ControlInfo> {
        let mut out: Vec<ControlInfo> = Vec::new();
        unsafe {
            EnumChildWindows(parent, Some(control_cb), &mut out as *mut Vec<ControlInfo> as LPARAM);
        }
        out
    }
}

/// 十六进制句柄 → HWND。外部 AI 传回来的就是 `0x...` 字符串。
#[cfg(windows)]
fn parse_hwnd(text: &str) -> Result<windows_sys::Win32::Foundation::HWND, String> {
    let trimmed = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    let value = u64::from_str_radix(trimmed, 16)
        .map_err(|_| format!("句柄不是合法的十六进制：{text}"))?;
    Ok(value as windows_sys::Win32::Foundation::HWND)
}

/// 顶层窗口列表。`only_visible` 过滤掉隐藏窗口（一般只要能看见的）。
pub fn list_windows(only_visible: bool) -> Vec<WindowInfo> {
    #[cfg(windows)]
    {
        let all = imp::list_windows();
        if only_visible {
            all.into_iter().filter(|w| w.visible).collect()
        } else {
            all
        }
    }
    #[cfg(not(windows))]
    {
        let _ = only_visible;
        Vec::new()
    }
}

/// 某个窗口下的子控件列表。
pub fn list_controls(hwnd: &str) -> Result<Vec<ControlInfo>, String> {
    #[cfg(windows)]
    {
        Ok(imp::list_controls(parse_hwnd(hwnd)?))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err("仅支持 Windows（项目边界）".into())
    }
}

/// 点击一个控件。
///
/// 优先 BM_CLICK（按钮的标准通知，程序能收到 OnClick）；非按钮控件退回
/// WM_LBUTTONDOWN + WM_LBUTTONUP 序列。
pub fn click(hwnd: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PostMessageW, SendMessageW, BM_CLICK, WM_LBUTTONDOWN, WM_LBUTTONUP,
        };
        let h = parse_hwnd(hwnd)?;
        let class = imp::class_of(h);
        if class.starts_with("Button") || class.starts_with("TButton") {
            unsafe {
                SendMessageW(h, BM_CLICK, 0, 0);
            }
            return Ok(format!("已点击按钮 {hwnd}（BM_CLICK）"));
        }
        unsafe {
            PostMessageW(h, WM_LBUTTONDOWN, 1, 0);
            PostMessageW(h, WM_LBUTTONUP, 0, 0);
        }
        Ok(format!("已点击控件 {hwnd}（{class}）"))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err("仅支持 Windows（项目边界）".into())
    }
}

/// 读控件文本。
pub fn read_text(hwnd: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        Ok(imp::window_text(parse_hwnd(hwnd)?))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err("仅支持 Windows（项目边界）".into())
    }
}

/// 设置控件文本（编辑框、下拉框等）。
pub fn set_text(hwnd: &str, text: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_SETTEXT};
        let h = parse_hwnd(hwnd)?;
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        unsafe {
            SendMessageW(h, WM_SETTEXT, 0, wide.as_ptr() as isize);
        }
        Ok(format!(
            "已写入 {len} 字符到 {hwnd}",
            len = text.chars().count()
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, text);
        Err("仅支持 Windows（项目边界）".into())
    }
}

/// 给控件发一次按键（默认回车；传虚拟键码即可）。
pub fn press_key(hwnd: &str, vk: u32) -> Result<String, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PostMessageW, SetForegroundWindow, WM_KEYDOWN, WM_KEYUP,
        };
        let h = parse_hwnd(hwnd)?;
        unsafe {
            SetForegroundWindow(h);
            PostMessageW(h, WM_KEYDOWN, vk as usize, 0);
            PostMessageW(h, WM_KEYUP, vk as usize, 0);
        }
        Ok(format!("已向 {hwnd} 发送按键 {vk}"))
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, vk);
        Err("仅支持 Windows（项目边界）".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::parse_hwnd;

    #[test]
    fn parses_hex_handle_with_and_without_prefix() {
        assert_eq!(parse_hwnd("0x1A2B").unwrap(), 0x1A2B);
        assert_eq!(parse_hwnd("1a2b").unwrap(), 0x1A2B);
        assert_eq!(parse_hwnd(" 0X10 ").unwrap(), 0x10);
    }

    #[test]
    fn rejects_non_hex_handle() {
        assert!(parse_hwnd("not-a-handle").is_err());
    }
}
