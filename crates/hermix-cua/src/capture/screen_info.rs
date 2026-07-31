//! `screen_info` — enumerate the connected displays.
//!
//! Currently returns **only the primary monitor**, queried via
//! `GetSystemMetrics(SM_CXSCREEN | SM_CYSCREEN)`. The result is
//! shaped as a `Vec<MonitorInfo>` so the wire payload stays stable.
//!
//! Multi-monitor enumeration (via `EnumDisplayMonitors` +
//! `GetMonitorInfo` to surface every connected display with the
//! correct position in the virtual desktop coordinate space) is
//! not yet implemented.

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::capture::MonitorInfo;
use crate::CuaResult;

/// Enumerate the connected monitors. Currently returns exactly one
/// entry (the primary monitor); multi-monitor enumeration is not
/// yet implemented.
pub fn enumerate() -> CuaResult<Vec<MonitorInfo>> {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    Ok(vec![MonitorInfo {
        index: 0,
        name: "primary".into(),
        x: 0,
        y: 0,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
        is_primary: true,
    }])
}