//! Screen capture primitives.
//!
//! Two backends are wired behind a single `CaptureBackend`:
//!
//! - **BitBlt** (Phase 1, current). Uses `GetDC(NULL)` +
//!   `CreateCompatibleBitmap` + `BitBlt(SRCCOPY)` to dump the
//!   primary screen, then re-encodes to PNG via `image`. Always
//!   works on any Windows 10/11 install with no extra runtime —
//!   the price is a full-screen `BitBlt` and a single GDI snapshot
//!   per call (no delta tracking).
//! - **DXGI Output Duplication** (Phase 2, stub). Will land once
//!   the DXGI swap chain path is validated. See
//!   [`dxgi::dxgi_capture`] for the planned surface.

pub mod dxgi;
pub mod screen_info;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::CuaError;
use crate::CuaResult;

/// Monitor descriptor returned by [`screen_info::enumerate`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorInfo {
    /// 0-based index in the enumeration order. `0` is always the
    /// primary monitor in Phase 1; multi-monitor ordering will
    /// land with the DXGI backend.
    pub index: usize,
    /// Human-readable device name (`"\\.\DISPLAY1"` on Windows).
    pub name: String,
    /// Top-left X coordinate in the virtual desktop coordinate
    /// space. `0` for the primary monitor in single-display setups.
    pub x: i32,
    /// Top-left Y coordinate.
    pub y: i32,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// `true` for the primary monitor (the one whose origin the
    /// taskbar lives on).
    pub is_primary: bool,
}

/// Result of a successful screenshot. The PNG bytes are encoded
/// once at capture time; callers can serialise
/// `png_base64` directly into the tool-result JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    /// Width / height of the captured frame, in pixels.
    pub width: u32,
    pub height: u32,
    /// Monitor index the frame was captured from (Phase 1:
    /// always `0`).
    pub monitor: usize,
    /// Human-readable monitor name (Phase 1: from
    /// `GetSystemMetrics`/primary resolution).
    pub monitor_name: String,
    /// Raw size of the encoded PNG in bytes. Exposed so the UI
    /// can render a "(2.4 MB)" badge next to a screenshot result.
    pub bytes: usize,
    /// Standard base64 (with `+`/`/` padding) of the PNG payload.
    pub png_base64: String,
}

/// Configuration for a single screenshot call.
#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    /// Which monitor to capture. Phase 1 ignores this and always
    /// captures the primary monitor — the field is preserved so
    /// the call signature is stable across the DXGI swap.
    pub monitor: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { monitor: 0 }
    }
}

/// Capture the primary monitor.
///
/// This is the entry point used by the `screenshot` tool handler.
/// The implementation dispatches to the DXGI backend in Phase 2
/// and falls back to BitBlt when DXGI is unavailable — public
/// signature is stable so the tool schema does not change.
///
/// ## DXGI-first, BitBlt-fallback rationale
///
/// DXGI (`IDXGIOutputDuplication`) is the right long-term answer
/// (5–10× faster, multi-monitor correct, no GDI contention) but
/// it has a handful of known failure modes the BitBlt path does
/// not have:
///
/// - `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` when the session is
///   in transition (lock screen, UAC prompt, RDP attach).
/// - Driver-level failures on a few virtualised GPUs (Hyper-V
///   session host, some VDI clients).
/// - Multi-adapter laptops where the active display is wired to
///   a different GPU than the adapter DXGI hands us.
///
/// A black-box agent tool that returns `Err` for any of these
/// would be a regression vs. Phase 1's BitBlt (which always
/// works on any Windows 10/11 install). So: try DXGI, on any
/// `CuaError::Dxgi(_)` fall back to BitBlt, and only return
/// `Err` if BitBlt also fails. The wire shape is identical to
/// either backend — the user never sees which path actually ran.
pub fn screenshot(config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    #[cfg(windows)]
    {
        match dxgi::dxgi_capture(config) {
            Ok(result) => Ok(result),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "DXGI capture failed, falling back to BitBlt"
                );
                bitblt_capture(config)
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err(CuaError::NotFound(
            "screenshot is only implemented on Windows".into(),
        ))
    }
}

/// Phase 1 BitBlt capture backend. Allocates a GDI bitmap of the
/// primary monitor's resolution, blits the desktop into it, then
/// re-encodes to PNG via `image::codecs::png::PngEncoder`.
#[cfg(windows)]
fn bitblt_capture(config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        SelectObject, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
    };

    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(0) as u32;
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(0) as u32;
    if width == 0 || height == 0 {
        return Err(CuaError::Gdi(format!(
            "GetSystemMetrics returned {}x{} (no display attached?)",
            width, height
        )));
    }

    unsafe {
        let screen_dc = GetDC(HWND(std::ptr::null_mut()));
        if screen_dc.is_invalid() {
            return Err(CuaError::Gdi("GetDC(NULL) returned NULL".into()));
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return Err(CuaError::Gdi("CreateCompatibleDC failed".into()));
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width as i32, height as i32);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return Err(CuaError::Gdi("CreateCompatibleBitmap failed".into()));
        }
        let prev = SelectObject(mem_dc, bitmap);
        let blit_ok = BitBlt(mem_dc, 0, 0, width as i32, height as i32, screen_dc, 0, 0, SRCCOPY).is_ok();
        if !blit_ok {
            let _ = SelectObject(mem_dc, prev);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return Err(CuaError::Gdi("BitBlt failed".into()));
        }

        // Dump the bitmap bits via `GetBitmapBits`. We allocate a
        // tight RGBA buffer; GDI gives us top-down 32bpp on the
        // platforms we target. The PNG encoder expects RGBA8.
        let buf_size = (width as usize) * (height as usize) * 4;
        let mut bits: Vec<u8> = vec![0u8; buf_size];
        let copied = windows::Win32::Graphics::Gdi::GetBitmapBits(
            bitmap,
            buf_size as i32,
            bits.as_mut_ptr() as *mut _,
        );
        if copied <= 0 {
            let _ = SelectObject(mem_dc, prev);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return Err(CuaError::Gdi(format!(
                "GetBitmapBits returned {copied} (expected {buf_size})"
            )));
        }
        bits.truncate(copied as usize);

        let _ = SelectObject(mem_dc, prev);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);

        // Encode RGBA → PNG.
        let mut png_buf: Vec<u8> = Vec::with_capacity(buf_size);
        {
            use image::{codecs::png::PngEncoder, ImageEncoder};
            let encoder = PngEncoder::new(&mut png_buf);
            encoder
                .write_image(&bits, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| CuaError::Png(e.to_string()))?;
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
        Ok(ScreenshotResult {
            width,
            height,
            monitor: config.monitor,
            monitor_name: "primary".into(),
            bytes: png_buf.len(),
            png_base64: b64,
        })
    }
}

/// Non-Windows stub: the function is reachable in the API
/// surface but always returns `NotImplemented` so cross-platform
/// CI does not panic. The `#[cfg(windows)]` `bitblt_capture`
/// is the real implementation; this `cfg(not(windows))` arm only
/// exists so the symbol is resolvable for `cargo check` outside
/// Windows.
#[cfg(not(windows))]
fn bitblt_capture(_config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    Err(CuaError::NotFound(
        "screenshot is only implemented on Windows in Phase 1".into(),
    ))
}