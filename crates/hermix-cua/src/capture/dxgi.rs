//! DXGI Output Duplication backend (the primary capture path).
//!
//! ## What this does
//!
//! On Windows 8+, `IDXGIOutputDuplication` exposes the GPU's
//! swap-chain surface as a `ID3D11Texture2D` we can read back.
//! Compared to the BitBlt path:
//!
//! - **Speed**: 5–10× faster on most hardware (BitBlt stalls on
//!   the GPU/CPU boundary; DXGI is a direct DMA path into a
//!   staging texture).
//! - **Multi-monitor correctness**: BitBlt only sees the primary
//!   monitor's dimensions via `GetSystemMetrics` and silently
//!   captures the *primary* surface even when the cursor is on
//!   another display. DXGI enumerates outputs and respects the
//!   monitor index the caller picks.
//! - **No GDI session contention**: GDI objects (DCs, bitmaps)
//!   fight with every other GUI process on the machine. DXGI
//!   uses GPU memory that no other user-mode process touches.
//!
//! ## Design
//!
//! Every call opens a fresh `ID3D11Device` + `IDXGIOutputDuplication`
//! pair, captures one frame, and releases everything. This is
//! wasteful per-call, but it's the simplest correct shape — a
//! long-lived duplication can be invalidated by lock screen, UAC
//! prompt, monitor hot-plug, RDP session attach, etc., and we'd
//! rather pay a 50–100 ms init cost per `cua_screenshot` than
//! re-implement the full "re-acquire on DXGI_ERROR_ACCESS_LOST"
//! dance .
//!
//! ## Fallback contract
//!
//! Any error here returns `Err(CuaError::Dxgi(_))`; the public
//! `screenshot()` wrapper in `super` catches that and falls back
//! to BitBlt so a single broken monitor never bricks the agent.
//!
//! ## Threading
//!
//! All COM / D3D calls run on the calling thread (which is
//! already inside `tokio::task::spawn_blocking` per the
//! `hermix-tools` wrapper).

use base64::Engine as _;
use windows::core::Interface as _;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter1, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_OUTDUPL_FRAME_INFO,
};

use crate::capture::{CaptureConfig, ScreenshotResult};
use crate::error::CuaError;
use crate::CuaResult;

/// Timeout for `IDXGIOutputDuplication::AcquireNextFrame` in
/// milliseconds. 100 ms is short enough that the agent loop
/// stays responsive on a busy desktop and long enough that the
/// first call (which has no cached frame) actually gets a
/// surface back.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// Capture the primary monitor via DXGI Output Duplication.
///
/// On success returns the PNG payload wrapped in a
/// [`ScreenshotResult`]. On any DXGI / D3D11 error returns
/// `Err(CuaError::Dxgi(_))` — the public `screenshot()` wrapper
/// in `super` falls back to BitBlt on this path so a broken
/// monitor driver never bricks the agent.
pub fn dxgi_capture(config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    unsafe { dxgi_capture_inner(config) }
}

unsafe fn dxgi_capture_inner(config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    // 1. Create the D3D11 device + immediate context. We request
    // BGRA support implicitly via the BGRA output format
    // (`DXGI_FORMAT_B8G8R8A8_UNORM`) — `D3D11CreateDevice`
    // fails if the GPU/driver combo cannot produce that.
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    let create_flags = windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_FLAG(0);
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE(std::ptr::null_mut()),
        create_flags,
        Some(&feature_levels),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    )
    .map_err(|e| CuaError::Dxgi(format!("D3D11CreateDevice failed: {e}")))?;
    let device = device.ok_or_else(|| CuaError::Dxgi("device is null after create".into()))?;
    let context = context.ok_or_else(|| CuaError::Dxgi("context is null after create".into()))?;

    // 2. Walk DXGI: device → adapter → output → output1.
    let dxgi_device: IDXGIDevice = device
        .cast()
        .map_err(|e| CuaError::Dxgi(format!("device.cast::<IDXGIDevice>: {e}")))?;
    // `GetAdapter` returns the v0 interface; we need v1 for
    // `EnumOutputs` so cast explicitly. (The same `IUnknown`
    // backs both; the cast is just a vtable check.)
    let adapter0: windows::Win32::Graphics::Dxgi::IDXGIAdapter =
        dxgi_device
            .GetAdapter()
            .map_err(|e| CuaError::Dxgi(format!("IDXGIDevice::GetAdapter: {e}")))?;
    let adapter: IDXGIAdapter1 = adapter0
        .cast()
        .map_err(|e| CuaError::Dxgi(format!("IDXGIAdapter.cast::<IDXGIAdapter1>: {e}")))?;
    // Pick the output the caller asked for. Out-of-range or
    // missing outputs are a "DXGI error" — the public wrapper
    // falls back to BitBlt, which only ever captures the primary
    // monitor anyway.
    let output_idx = config.monitor as u32;
    let output = adapter
        .EnumOutputs(output_idx)
        .map_err(|e| CuaError::Dxgi(format!("IDXGIAdapter1::EnumOutputs({output_idx}): {e}")))?;
    let output1: IDXGIOutput1 = output
        .cast()
        .map_err(|e| CuaError::Dxgi(format!("IDXGIOutput.cast::<IDXGIOutput1>: {e}")))?;

    // 3. Create the duplication. This call can fail with
    // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE when the session
    // is in a transition (lock screen, UAC, RDP attach) —
    // fall back to BitBlt in that case.
    let duplication: IDXGIOutputDuplication = output1
        .DuplicateOutput(&device)
        .map_err(|e| CuaError::Dxgi(format!("IDXGIOutput1::DuplicateOutput: {e}")))?;

    // 4. Acquire the next frame. 100ms is short enough to keep
    // the agent loop snappy and long enough that the very
    // first call (no cached frame) gets a real surface.
    let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut desktop_resource: Option<IDXGIResource> = None;
    duplication
        .AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut desktop_resource)
        .map_err(|e| CuaError::Dxgi(format!("AcquireNextFrame: {e}")))?;
    let desktop_resource = desktop_resource
        .ok_or_else(|| CuaError::Dxgi("AcquireNextFrame returned no desktop resource".into()))?;

    // RAII guard: ensure ReleaseFrame is called even when an
    // early-return fires below. Without this, a partial failure
    // path could leave the duplication locked and brick the
    // next `cua_screenshot` call.
    struct FrameReleaser(IDXGIOutputDuplication);
    impl Drop for FrameReleaser {
        fn drop(&mut self) {
            // `ReleaseFrame` returns an HRESULT we intentionally
            // ignore — once we're in a Drop handler the caller
            // has already given up on the result.
            let _ = unsafe { self.0.ReleaseFrame() };
        }
    }
    let _releaser = FrameReleaser(duplication);

    // 5. Downcast the desktop resource to a texture and read its
    // dimensions / format. DXGI Output Duplication always
    // hands back a `ID3D11Texture2D` in
    // `DXGI_FORMAT_B8G8R8A8_UNORM`.
    let texture: ID3D11Texture2D = desktop_resource
        .cast()
        .map_err(|e| CuaError::Dxgi(format!("desktop_resource.cast::<ID3D11Texture2D>: {e}")))?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    let width = desc.Width;
    let height = desc.Height;
    if width == 0 || height == 0 {
        return Err(CuaError::Dxgi(format!(
            "desktop texture has zero dimensions ({}x{})",
            width, height
        )));
    }

    // 6. Build a CPU-readable staging texture that mirrors the
    // desktop texture's geometry. Staging textures must have
    // `D3D11_USAGE_STAGING` and `D3D11_CPU_ACCESS_READ`; they
    // cannot be bound to the pipeline, but they CAN be the
    // target of `CopyResource` and `Map`.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: desc.SampleDesc,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        // `D3D11_CPU_ACCESS_READ` (= 0x0000_4000). The field is a
        // raw `u32` in windows 0.58 (the bit-flag newtype lives
        // only inside the helper API surface); we pass the
        // constant directly so the value is stable if the
        // D3D11_CPU_ACCESS_* set ever expands.
        CPUAccessFlags: 0x0000_4000,
        MiscFlags: 0,
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    device
        .CreateTexture2D(&staging_desc, None, Some(&mut staging))
        .map_err(|e| CuaError::Dxgi(format!("CreateTexture2D(staging): {e}")))?;
    let staging = staging.ok_or_else(|| CuaError::Dxgi("staging texture is null".into()))?;

    // 7. CopyResource moves the desktop pixels into the staging
    // texture. This is a GPU-side DMA — no CPU involvement
    // yet, which is why DXGI is so much faster than BitBlt.
    context.CopyResource(&staging, &texture);

    // 8. Map the staging texture. After this returns the GPU
    // has finished writing (or it failed, in which case the
    // Map's Err return tells us why). The mapped pointer is
    // valid until Unmap.
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    context
        .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .map_err(|e| CuaError::Dxgi(format!("ID3D11DeviceContext::Map: {e}")))?;

    // Build the RGBA8 byte stream the PNG encoder expects. The
    // DXGI desktop format is `B8G8R8A8_UNORM` — BGRA on disk,
    // four bytes per pixel. `RowPitch` may exceed `width * 4`
    // (the GPU is allowed to pad rows for alignment), so we
    // copy one row at a time.
    let row_pitch = mapped.RowPitch as usize;
    let width_us = width as usize;
    let height_us = height as usize;
    let src_pitch_size = row_pitch * height_us;
    // SAFETY: `pData` is a valid pointer to `RowPitch * Height`
    // bytes, written by the GPU just before `Map` returned. We
    // copy out of it before `Unmap` invalidates the pointer, and
    // we never write through it.
    let src_rows = std::slice::from_raw_parts(mapped.pData as *const u8, src_pitch_size);
    let mut rgba: Vec<u8> = Vec::with_capacity(width_us * height_us * 4);
    for y in 0..height_us {
        let row = &src_rows[y * row_pitch..y * row_pitch + width_us * 4];
        // Swap B ↔ R per pixel; G and A stay put.
        let mut i = 0;
        while i + 4 <= row.len() {
            rgba.push(row[i + 2]); // R = src.B
            rgba.push(row[i + 1]); // G = src.G
            rgba.push(row[i]); // B = src.R
            rgba.push(row[i + 3]); // A = src.A
            i += 4;
        }
    }
    // SAFETY: explicit Unmap — the FrameReleaser above would
    // drop the duplication without the unmap, but the staging
    // texture still needs Unmap for the GPU to consider the
    // copy complete.
    context.Unmap(&staging, 0);

    // 9. Encode RGBA8 → PNG. The encoder is the same one
    // `bitblt_capture` uses, so the wire format is identical
    // to the BitBlt path and downstream consumers don't notice the
    // backend swap.
    let mut png_buf: Vec<u8> = Vec::with_capacity(width_us * height_us);
    {
        use image::{codecs::png::PngEncoder, ImageEncoder};
        PngEncoder::new(&mut png_buf)
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| CuaError::Png(e.to_string()))?;
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
    Ok(ScreenshotResult {
        width,
        height,
        monitor: config.monitor,
        monitor_name: format!("output{output_idx}"),
        bytes: png_buf.len(),
        png_base64: b64,
    })
}

// ---------------------------------------------------------------------------
// Non-Windows stub: matches the existing `bitblt_capture` shape so
// `super::screenshot` always has a `dxgi_capture` symbol available.
// ---------------------------------------------------------------------------
#[cfg(not(windows))]
pub fn dxgi_capture(_config: CaptureConfig) -> CuaResult<ScreenshotResult> {
    Err(CuaError::NotFound(
        "DXGI capture is only implemented on Windows".into(),
    ))
}

// ---------------------------------------------------------------------------
// Tests — only the dispatch shape is portable. Real device /
// adapter / output wiring requires a live Windows desktop, which
// CI on macOS / Linux does not have.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the public surface. If a future refactor changes the
    /// function signature the dispatch in `super::screenshot`
    /// breaks silently, so the test fails before it reaches the
    /// wire.
    #[test]
    fn function_signature_is_stable() {
        let f: fn(CaptureConfig) -> CuaResult<ScreenshotResult> = dxgi_capture;
        let _ = f;
    }
}
