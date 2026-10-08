// 截图核心：抓屏 → 覆盖窗选区 → 裁剪 → 剪贴板。
// 方案：docs/notes/implemented/feature/2026-10-07-screenshot-tool.md
// 关键模型：先抓后盖——进入覆盖窗前完成抓屏，用户在"照片"上画框，
// 覆盖窗自身永远不会被截入。
//
// 平台：Windows-only（项目纪律，无跨平台目标）。非 Windows 编译为
// NotFound 桩，与 commands_cua 同一模式。

use serde::Serialize;

/// 选区（覆盖窗回传）：物理像素，全局虚拟屏幕坐标。
#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 一次抓屏会话的缓存：原始位图 + 所在显示器原点（把覆盖窗本地
/// 坐标换算回位图像素用）。存在 static 里，覆盖窗关闭即清。
pub struct CaptureSession {
    /// BGRA，物理像素，行序 top-down，宽 = width
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 该位图对应显示器左上角在虚拟屏幕坐标系的位置
    pub origin_x: i32,
    pub origin_y: i32,
}

static SESSION: once_cell::sync::OnceCell<std::sync::Mutex<Option<CaptureSession>>> =
    once_cell::sync::OnceCell::new();

pub(crate) fn session() -> &'static std::sync::Mutex<Option<CaptureSession>> {
    SESSION.get_or_init(|| std::sync::Mutex::new(None))
}

// ── 裁剪（纯函数，可单测）─────────────────────────────────────────

/// 从缓存位图裁出选区，返回 RGBA 字节（image crate 编码 PNG 用）。
/// 选区坐标为虚拟屏幕坐标；位图有自身 origin，先平移再钳位。
/// 坐标不匹配（选区完全在位图外）返回 None —— 调用方报错，不静默。
pub fn crop_rgba(session: &CaptureSession, sel: Selection) -> Option<Vec<u8>> {
    let sx = (sel.x - session.origin_x).max(0) as u32;
    let sy = (sel.y - session.origin_y).max(0) as u32;
    let ex = ((sel.x - session.origin_x) + sel.width).min(session.width as i32).max(0) as u32;
    let ey = ((sel.y - session.origin_y) + sel.height).min(session.height as i32).max(0) as u32;
    if sx >= ex || sy >= ey {
        return None;
    }
    let (w, h) = (ex - sx, ey - sy);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    // xcap 位图为 BGRA top-down；裁剪同时交换 R/B → RGBA。
    for row in sy..ey {
        let row_start = ((row * session.width + sx) * 4) as usize;
        for px in 0..w {
            let base = row_start + (px * 4) as usize;
            let b = session.pixels[base];
            let g = session.pixels[base + 1];
            let r = session.pixels[base + 2];
            let a = session.pixels[base + 3];
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(out)
}

/// BGRA→RGBA 整帧转换（覆盖窗幕布用 data URL 显示全屏照片）。
pub fn bgra_to_rgba(pixels: &[u8]) -> Vec<u8> {
    let mut out = pixels.to_vec();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

// ── 抓屏 ─────────────────────────────────────────────────────────

#[cfg(windows)]
pub fn capture_monitor_at_cursor() -> Result<CaptureSession, String> {
    // 光标所在显示器优先；取不到光标则用主显示器。
    // xcap 0.9 的 getter 全部返回 XCapResult，失败视为不可用跳过。
    let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
    let cursor = cursor_pos();
    let monitor = monitors
        .iter()
        .find(|m| {
            let (Ok(mx), Ok(my)) = (m.x(), m.y()) else {
                return false;
            };
            let (Ok(mw), Ok(mh)) = (m.width(), m.height()) else {
                return false;
            };
            cursor.is_some_and(|(cx, cy)| {
                cx >= mx && cx < mx + mw as i32 && cy >= my && cy < my + mh as i32
            })
        })
        .or_else(|| monitors.first())
        .ok_or("没有可用显示器")?
        .clone();

    let image = monitor
        .capture_image()
        .map_err(|e| format!("抓屏失败: {e}"))?;
    // xcap 返回 RGBA（RgbaImage）；统一转 BGRA 存缓存，裁剪时再转回，
    // 保持 crop_rgba 单一数据通路（BGRA ↔ RGBA 换算只在这一对函数里）。
    let mut pixels = Vec::with_capacity(image.as_raw().len());
    for px in image.as_raw().chunks_exact(4) {
        pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    let (Ok(ox), Ok(oy)) = (monitor.x(), monitor.y()) else {
        return Err("显示器坐标获取失败".into());
    };
    Ok(CaptureSession {
        width: image.width(),
        height: image.height(),
        origin_x: ox,
        origin_y: oy,
        pixels,
    })
}

#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}

// ── 窗口识别（几何法：光标下顶层 HWND 的 rect）──────────────────

#[derive(Debug, Clone, Serialize)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[cfg(windows)]
pub fn window_rect_at(cursor: (i32, i32)) -> Option<WindowRect> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetWindowRect, IsWindowVisible, WindowFromPoint, GA_ROOT,
    };

    let pt = POINT { x: cursor.0, y: cursor.1 };
    unsafe {
        let hit = WindowFromPoint(pt);
        if hit.0.is_null() {
            return None;
        }
        let root = GetAncestor(hit, GA_ROOT);
        if root.0.is_null() || !IsWindowVisible(root).as_bool() {
            return None;
        }
        // cloak 的 UWP 幽灵窗（如搜索宿主）不参与识别
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            HWND(root.0),
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
        }
        let mut rect = RECT::default();
        if GetWindowRect(root, &mut rect).is_err() {
            return None;
        }
        // 覆盖窗自身也不参与（用户画到哪都指到覆盖窗上）
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        Some(WindowRect { x: rect.left, y: rect.top, width: w, height: h })
    }
}

// ── 剪贴板直写（CF_DIB + 注册格式 "PNG" 双格式）─────────────────
// 裸 user32/kernel32 FFI 而非 windows crate：依赖树里 webview2-com 拉
// windows-core 0.62、windows 0.58 拉 0.60，SetClipboardData 的泛型参数
// 在双版本下解析失败（E0271/E0277）。40 行 FFI 零版本冲突。
#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;
    pub type Handle = *mut c_void;
    #[link(name = "user32")]
    extern "system" {
        pub fn OpenClipboard(hwndnewowner: Handle) -> i32;
        pub fn EmptyClipboard() -> i32;
        pub fn SetClipboardData(uformat: u32, hmem: Handle) -> Handle;
        pub fn CloseClipboard() -> i32;
        pub fn RegisterClipboardFormatW(lpszformat: *const u16) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GlobalAlloc(uflags: u32, dwbytes: usize) -> Handle;
        pub fn GlobalLock(hmem: Handle) -> *mut u8;
        pub fn GlobalUnlock(hmem: Handle) -> i32;
    }
}

#[cfg(windows)]
pub fn copy_png_to_clipboard(rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    const CF_DIB: u32 = 8; // 标准剪贴板格式号
    const GMEM_MOVEABLE: u32 = 0x0002;

    // BITMAPINFOHEADER（负高 = top-down，免翻转）+ BGRA 像素。
    let header_size = 40usize;
    let pixel_size = rgba.len();
    let mut dib = Vec::with_capacity(header_size + pixel_size);
    dib.extend_from_slice(&(header_size as u32).to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    dib.extend_from_slice(&(-(height as i32)).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes()); // planes
    dib.extend_from_slice(&32u16.to_le_bytes()); // bpp
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&(pixel_size as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // ppm x
    dib.extend_from_slice(&0i32.to_le_bytes()); // ppm y
    dib.extend_from_slice(&0u32.to_le_bytes()); // clr used
    dib.extend_from_slice(&0u32.to_le_bytes()); // clr important
    for px in rgba.chunks_exact(4) {
        dib.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // RGBA→BGRA
    }

    let png_bytes = encode_png(rgba, width, height)?;

    let alloc = |data: &[u8]| -> Result<ffi::Handle, String> {
        unsafe {
            let h = ffi::GlobalAlloc(GMEM_MOVEABLE, data.len());
            if h.is_null() {
                return Err("GlobalAlloc 失败".into());
            }
            let p = ffi::GlobalLock(h);
            if p.is_null() {
                return Err("GlobalLock 失败".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            ffi::GlobalUnlock(h);
            Ok(h)
        }
    };

    // 重试 3 次：云剪贴板等工具可能短期占用剪贴板。
    let mut last_err = String::new();
    for _ in 0..3 {
        let ok = unsafe {
            if ffi::OpenClipboard(std::ptr::null_mut()) == 0 {
                last_err = "OpenClipboard 失败（被占用?）".into();
                false
            } else {
                ffi::EmptyClipboard();
                let mut ok = false;
                match alloc(&dib) {
                    Err(e) => last_err = e,
                    Ok(dib_h) => {
                        if ffi::SetClipboardData(CF_DIB, dib_h).is_null() {
                            last_err = "SetClipboardData(CF_DIB) 失败".into();
                        } else {
                            ok = true;
                            let name: Vec<u16> = "PNG\0".encode_utf16().collect();
                            let fmt = ffi::RegisterClipboardFormatW(name.as_ptr());
                            if fmt != 0 {
                                match alloc(&png_bytes) {
                                    Err(e) => {
                                        ok = false;
                                        last_err = e;
                                    }
                                    Ok(png_h) => {
                                        if ffi::SetClipboardData(fmt, png_h).is_null() {
                                            ok = false;
                                            last_err = "SetClipboardData(PNG) 失败".into();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                ffi::CloseClipboard();
                ok
            }
        };
        if ok {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(format!("剪贴板写入失败（已重试 3 次）: {last_err}"))
}

pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or("像素长度与尺寸不匹配")?
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_2x2() -> CaptureSession {
        // 2×2 BGRA：像素 (0,0)=蓝 (1,0)=绿 (0,1)=红 (1,1)=白
        CaptureSession {
            pixels: vec![
                255, 0, 0, 255, // B G R A
                0, 255, 0, 255,
                0, 0, 255, 255,
                255, 255, 255, 255,
            ],
            width: 2,
            height: 2,
            origin_x: 100,
            origin_y: 200,
        }
    }

    #[test]
    fn crop_full() {
        let s = session_2x2();
        let sel = Selection { x: 100, y: 200, width: 2, height: 2 };
        let out = crop_rgba(&s, sel).unwrap();
        assert_eq!(out.len(), 16);
        // 左上像素 BGRA(255,0,0) → RGBA(0,0,255)
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        // 右下 BGRA(255,255,255) → RGBA 白
        assert_eq!(&out[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn crop_subregion_with_offset() {
        let s = session_2x2();
        // 虚拟屏幕坐标平移：位图原点 (100,200)
        let sel = Selection { x: 101, y: 201, width: 1, height: 1 };
        let out = crop_rgba(&s, sel).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(&out[0..4], &[255, 255, 255, 255]); // (1,1) 白
    }

    #[test]
    fn crop_clamps_partial_overlap() {
        let s = session_2x2();
        // 选区越过位图右/下边界 → 钳位到 1×1
        let sel = Selection { x: 101, y: 201, width: 50, height: 50 };
        let out = crop_rgba(&s, sel).unwrap();
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn crop_outside_returns_none() {
        let s = session_2x2();
        let sel = Selection { x: 0, y: 0, width: 10, height: 10 };
        assert!(crop_rgba(&s, sel).is_none());
    }

    #[test]
    fn bgra_to_rgba_swaps() {
        let out = bgra_to_rgba(&[1, 2, 3, 4]);
        assert_eq!(out, vec![3, 2, 1, 4]);
    }
}
