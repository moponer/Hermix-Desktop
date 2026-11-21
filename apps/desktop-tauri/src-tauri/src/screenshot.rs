// 截图核心：抓屏 → 覆盖窗选区 → 裁剪 → 剪贴板。
// 方案：docs/notes/implemented/feature/2026-10-07-screenshot-tool.md
// 重写：docs/notes/implemented/feature/2026-10-23-screenshot-rewrite.md
//
// 关键模型：先抓后盖——进入覆盖窗前完成抓屏，用户在"照片"上画框，
// 覆盖窗自身永远不会被截入。
//
// 2026-10-23 结构重写（三处根修）：
// ① 抓屏换掉 xcap。xcap 逐显示器抓帧（~200ms）+ RGBA↔BGRA 全帧转换，
//    且只覆盖光标所在的那台显示器——多屏时其余区域天然是黑的。
//    现在一次 GDI BitBlt 抓整块虚拟桌面（SRCCOPY|CAPTUREBLT，~10ms）：
//    - 位图像素与虚拟桌面物理像素 1:1（原点对齐，零换算）；
//    - 多显示器一次覆盖，屏与屏之间没有黑缝;
//    - CAPTUREBLT 把分层窗口（半透明 UI）一并抓进来，缺它那些区域是黑块。
// ② 缩放换算不再由 Rust 猜（xcap 的 scale_factor 与 Windows 实际 DPI
//    未必一致，猜错就是「照片和屏幕对不齐」的黑边）。改由网页自己报
//    `devicePixelRatio`——它就是 WebView2 实际用的那个值。
// ③ 窗口吸附的 rect 与标题一并返回（原来只有 rect，界面上无法说明
//    「吸到的是哪个窗口」）。
//
// 平台：Windows-only（项目纪律，无跨平台目标）。非 Windows 编译为
// NotFound 桩，与 commands_cua 同一模式。
//
// 幕布显示已下沉原生分层窗（backdrop.rs，2026-10-24）：抓出的 BGRA 原样
// 直贴置顶窗，不编码不跨进程。本文件只剩裁剪/剪贴板/识别等纯像素与
// 几何逻辑。

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
    /// 该位图对应虚拟桌面左上角在虚拟屏幕坐标系的位置（物理像素）
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
    // 位图为 BGRA top-down；裁剪同时交换 R/B → RGBA。
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

// ── 抓屏（整块虚拟桌面）──────────────────────────────────────────

/// 虚拟桌面矩形（物理像素，左上角 + 尺寸）。多显示器时是所有显示器的
/// 并集——抓屏与覆盖窗都用这一个矩形，两者天然对齐。
#[cfg(windows)]
pub fn virtual_screen_rect() -> (i32, i32, u32, u32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        (x, y, w as u32, h as u32)
    }
}

/// 抓整块虚拟桌面 → BGRA 像素（top-down，物理像素）。
///
/// 一次 BitBlt 覆盖所有显示器：比「逐台显示器抓 + 拼接」少一次拷贝，
/// 也少一次坐标系拼接（拼接错位就是黑缝）。
#[cfg(windows)]
pub fn capture_desktop() -> Result<CaptureSession, String> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
        ROP_CODE, SRCCOPY,
    };

    let (ox, oy, w, h) = virtual_screen_rect();
    let (wi, hi) = (w as i32, h as i32);

    unsafe {
        // NULL = 整屏 DC。
        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            return Err("GetDC 取屏幕设备上下文失败".into());
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            ReleaseDC(HWND::default(), screen_dc);
            return Err("CreateCompatibleDC 失败".into());
        }

        // 负 biHeight = top-down，行序与屏幕一致，省一次翻转。
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: wi,
                biHeight: -hi,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [Default::default(); 1],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(
            mem_dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE::default(),
            0,
        ) {
            Ok(d) if !d.is_invalid() => d,
            _ => {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(HWND::default(), screen_dc);
                return Err("CreateDIBSection 失败（内存不足或权限不足）".into());
            }
        };
        let old = SelectObject(mem_dc, dib);

        // CAPTUREBLT 必须带：否则分层窗口（半透明 UI）区域抓回来是黑块。
        let rop = ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
        let copied = BitBlt(mem_dc, 0, 0, wi, hi, screen_dc, ox, oy, rop).is_ok();

        let mut pixels = Vec::new();
        if copied && !bits.is_null() {
            let len = (w as usize) * (h as usize) * 4;
            pixels.extend_from_slice(std::slice::from_raw_parts(bits as *const u8, len));
        }

        SelectObject(mem_dc, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);

        if !copied {
            return Err("BitBlt 抓屏失败（锁屏 / 受保护桌面？）".into());
        }
        if pixels.len() != (w as usize) * (h as usize) * 4 {
            return Err("抓屏像素长度异常".into());
        }

        Ok(CaptureSession {
            width: w,
            height: h,
            origin_x: ox,
            origin_y: oy,
            pixels,
        })
    }
}

// ── 窗口识别（几何法：光标下顶层 HWND 的 rect）──────────────────

#[derive(Debug, Clone, Serialize)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 窗口标题（空 = 无标题窗口）
    pub title: String,
}

#[cfg(windows)]
pub fn window_rect_at(cursor: (i32, i32), exclude: &[isize]) -> Option<WindowRect> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, IsIconic,
        IsWindowVisible,
    };

    // EnumWindows 按 Z 序（顶到底）枚举顶层窗口：跳过覆盖窗自身与原生幕布窗
    // （都是全屏置顶，不排除的话永远先命中它们），取第一个可见、未最小化、
    // 未 cloak 且包含该点的窗口。不能用 WindowFromPoint——它只给最顶窗口。
    struct Ctx<'a> {
        pt: POINT,
        exclude: &'a [isize],
        result: Option<WindowRect>,
    }
    unsafe extern "system" fn probe(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        if ctx.result.is_some() {
            return false.into();
        }
        if ctx.exclude.contains(&(hwnd.0 as isize)) {
            return true.into();
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true.into();
        }
        let mut rect = RECT::default();
        // 2026-11-19 用户实拍：GetWindowRect 返回**含不可见缩放贴边**的矩形
        // （Win10/11 左右下各 ~7px 物理像素）—— 吸附框右边和底部多出一截
        // 不贴边，截图也把左边贴边带进来。DWMWA_EXTENDED_FRAME_BOUNDS 给
        // 的是可见边框（与 DWM 阴影内沿对齐），才是用户眼中的"窗口边"。
        let ok = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
        }
        .is_ok()
            && rect.right > rect.left
            && rect.bottom > rect.top;
        if !ok && GetWindowRect(hwnd, &mut rect).is_err() {
            return true.into();
        }
        let (l, t, r, b) = (rect.left, rect.top, rect.right, rect.bottom);
        if r <= l || b <= t {
            return true.into();
        }
        // cloak 的 UWP 幽灵窗（如搜索宿主）不参与识别
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return true.into();
        }
        if ctx.pt.x < l || ctx.pt.x >= r || ctx.pt.y < t || ctx.pt.y >= b {
            return true.into();
        }
        ctx.result = Some(WindowRect {
            x: l,
            y: t,
            width: r - l,
            height: b - t,
            title: window_title(hwnd),
        });
        false.into() // 命中即停
    }

    unsafe {
        let mut ctx = Ctx {
            pt: POINT { x: cursor.0, y: cursor.1 },
            exclude,
            result: None,
        };
        let _ = EnumWindows(Some(probe), LPARAM(&mut ctx as *mut Ctx as isize));
        ctx.result
    }
}

/// 窗口标题（UTF-16 → UTF-8，失败/空标题返回空串）。
#[cfg(windows)]
fn window_title(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// 当前前台窗口句柄（截图结束后把焦点还给原窗口）。
#[cfg(windows)]
pub fn foreground_window() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let h = GetForegroundWindow();
        if h.0.is_null() {
            None
        } else {
            Some(h.0 as isize)
        }
    }
}

/// 把焦点还给指定窗口（截完图回到原工作流，别让用户手动切回去）。
#[cfg(windows)]
pub fn restore_foreground(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    unsafe {
        let _ = SetForegroundWindow(HWND(hwnd as *mut std::ffi::c_void));
    }
}

/// ESC 是否处于按下状态。覆盖窗失去键盘焦点时 webview keydown 收不到，
/// 控制器用它做兜底退出（GetAsyncKeyState 是系统级状态，与焦点无关）。
#[cfg(windows)]
pub fn esc_pressed() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_ESCAPE: i32 = 0x1B;
    unsafe { (GetAsyncKeyState(VK_ESCAPE) as u16) & 0x8000 != 0 }
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

/// 幕布显示不再需要 JPEG（原生分层窗直贴原始 BGRA，见 backdrop.rs）；
/// PNG 仍服务两条无损路径：`screenshot_select` 的编辑底图与剪贴板。

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
}
