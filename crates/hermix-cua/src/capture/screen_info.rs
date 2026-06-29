//! `screen_info` — enumerate the connected displays.
//!
//! Phase 1 returns **only the primary monitor**, queried via
//! `GetSystemMetrics(SM_CXSCREEN | SM_CYSCREEN)`. The result is
//! shaped the same way the Phase 2 DXGI backend will return
//! (`Vec<MonitorInfo>`) so the wire payload never changes.
//!
//! Phase 2 will swap the body for
//! `EnumDisplayMonitors` + `GetMonitorInfo` to surface every
//! connected display with the correct position in the virtual
//! desktop coordinate space.

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::capture::MonitorInfo;
use crate::CuaResult;

/// Enumerate the connected monitors. Phase 1 returns exactly one
/// entry (the primary monitor). Phase 2 will return one entry
/// per physical display.
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