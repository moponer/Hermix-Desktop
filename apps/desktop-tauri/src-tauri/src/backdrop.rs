// 原生幕布窗：截图冻结帧的 Win32 分层窗口（UpdateLayeredWindow 直贴）。
// 依据：docs/notes/implemented/feature/2026-10-24-screenshot-native-backdrop.md
//
// 为什么幕布必须离开 webview：幕布的唯一使命是「冻结画面当靶子」，但像素进
// WebView 只能是字符串（编码 → base64 → JSON 跨进程 → 浏览器解码），4K 下这条
// 链路 ~200ms；微信的幕布是同进程分层窗直贴，~15ms。本模块把幕布挪回原生层：
// BitBlt 抓出来的 BGRA 补 alpha 后一次 UpdateLayeredWindow 上屏，不编码、
// 不序列化、不过进程边界，4K 全链路（抓屏+绘制）~20-35ms，与微信同一机制。
//
// 职责边界：本窗只显示冻结帧（一张位图，无交互）。选区吸附/框选/工具条/
// 编辑器全部留在 webview（透明，浮在本窗之上）——原生窗整体替代 webview 的
// 方案已在 2026-10-23 笔记否决（编辑器原生重写成本过高）。
//
// 线程模型：窗口在主线程创建（tao 事件泵为同线程所有窗口派发消息，本模块
// 不自建消息循环），所有触碰（paint/hide）必须经 `run_on_main_thread` 派发。
// paint 在主线程停 5-15ms（4K 的 ULW 拷贝）——一次性成本，与微信同款做法。
//
// windows crate 锁 0.58（见 Cargo.toml 注记：与 windows-core 0.62 双版本共存，
// 本模块只用 0.58 自洽的类型，不与 0.62 的 webview2-com 类型交叉）。

use once_cell::sync::OnceCell;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    HMENU, HWND_TOPMOST, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, ULW_ALPHA, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "HermixScreenshotBackdrop";

/// 分层窗 + 当前的 ULW 位图。DIB 跨会话复用（虚拟桌面尺寸变了才重建），
/// 位图指针由 CreateDIBSection 给出、DeleteObject 前一直有效。
struct BackdropWindow {
    hwnd: HWND,
    dc_mem: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    width: u32,
    height: u32,
}

// 仅主线程触碰（经 run_on_main_thread 派发）；句柄与裸指针的线程约束由
// 这个派发纪律保证，而非 Send 语义本身。
unsafe impl Send for BackdropWindow {}

static BACKDROP: OnceCell<std::sync::Mutex<Option<BackdropWindow>>> = OnceCell::new();

fn slot() -> &'static std::sync::Mutex<Option<BackdropWindow>> {
    BACKDROP.get_or_init(|| std::sync::Mutex::new(None))
}

/// 幕布窗存在时返回句柄（isize）——窗口吸附识别要排除它。
pub fn hwnd() -> Option<isize> {
    slot().lock().unwrap().as_ref().map(|b| b.hwnd.0 as isize)
}

/// 隐藏幕布窗（幂等；未创建时是空操作）。经 run_on_main_thread 派发调用。
pub fn hide() {
    if let Some(b) = slot().lock().unwrap().as_ref() {
        unsafe {
            let _ = ShowWindow(b.hwnd, SW_HIDE);
        }
    }
}

/// 确保幕布窗存在（主线程调用；幂等）。预热用——0×0 隐藏窗只完成类注册
/// 与句柄占位，ULW 位图在首次 paint 时按虚拟桌面尺寸建。
pub fn ensure_created() -> Result<(), String> {
    let mut guard = slot().lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    *guard = Some(unsafe { create_window()? });
    Ok(())
}

/// 把幕布画上屏并显示（主线程调用）。
///
/// `bgra` 是 `capture_desktop()` 的原始位图（BGRA、top-down、物理像素），
/// 原样直贴 —— 幕布分辨率 = 屏幕分辨率，没有编码也没有降采样。
/// UpdateLayeredWindow 的 pptDst 用虚拟桌面原点，顺带完成窗口定位。
pub fn paint_and_show(
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    bgra: &[u8],
) -> Result<(), String> {
    let len = (width as usize) * (height as usize) * 4;
    if bgra.len() != len {
        return Err(format!("幕布像素长度不符: {} ≠ {len}", bgra.len()));
    }
    let mut guard = slot().lock().unwrap();
    if guard.is_none() {
        *guard = Some(unsafe { create_window()? });
    }
    let b = guard.as_mut().unwrap();
    unsafe {
        rebuild_dib_if_resized(b, width, height)?;

        // BitBlt 抓屏的 alpha 字节不可靠（GDI 不保证写 0xFF），而 ULW 按
        // **预乘 alpha** 解释：alpha=0 的像素整窗隐形。抓屏内容恒不透明，
        // 拷入 ULW 位图时统一补 255。
        let dst = std::slice::from_raw_parts_mut(b.bits, len);
        copy_bgra_opaque(bgra, dst);

        // ULW：位图即窗面，pptDst（屏幕坐标）顺带定位窗口。
        let hdc_dst = GetDC(b.hwnd);
        let ppt_dst = POINT { x: origin_x, y: origin_y };
        let size = SIZE { cx: width as i32, cy: height as i32 };
        let ppt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ok = UpdateLayeredWindow(
            b.hwnd,
            hdc_dst,
            Some(&ppt_dst),
            Some(&size),
            b.dc_mem,
            Some(&ppt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(b.hwnd, hdc_dst);
        ok.map_err(|e| format!("UpdateLayeredWindow 失败: {e}"))?;

        // 重新进入 TOPMOST 带（webview 之上由调用方再抬 webview），不抢焦点。
        SetWindowPos(
            b.hwnd,
            HWND_TOPMOST,
            origin_x,
            origin_y,
            width as i32,
            height as i32,
            SWP_NOACTIVATE,
        )
        .map_err(|e| format!("SetWindowPos 失败: {e}"))?;
        let _ = ShowWindow(b.hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

/// 虚拟桌面尺寸变了（插拔显示器 / 改分辨率）→ 重建 DIB。
///
/// # Safety
/// `b.bits` 必须仍指向上次 CreateDIBSection 的位图（结构不变式）。
unsafe fn rebuild_dib_if_resized(b: &mut BackdropWindow, width: u32, height: u32) -> Result<(), String> {
    use std::ffi::c_void;

    if b.width == width && b.height == height {
        return Ok(());
    }
    if !b.dc_mem.is_invalid() {
        let _ = DeleteDC(b.dc_mem);
    }
    if !b.dib.is_invalid() {
        let _ = DeleteObject(b.dib);
    }
    let screen_dc = GetDC(HWND::default());
    let mem_dc = CreateCompatibleDC(screen_dc);
    ReleaseDC(HWND::default(), screen_dc);
    if mem_dc.is_invalid() {
        return Err("CreateCompatibleDC 失败".into());
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // 负高 = top-down，与抓屏行序一致
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        .map_err(|e| format!("CreateDIBSection 失败: {e}"))?;
    if dib.is_invalid() || bits.is_null() {
        return Err("CreateDIBSection 返回空位图".into());
    }
    // 位图保持选中进 dc_mem（ULW 的 hdcSrc 就是它），DC 销毁时自然解选。
    let _ = SelectObject(mem_dc, dib);
    b.dc_mem = mem_dc;
    b.dib = dib;
    b.bits = bits as *mut u8;
    b.width = width;
    b.height = height;
    Ok(())
}

/// BGRA 拷贝并强制 alpha = 255（见 paint_and_show 注记）。
pub fn copy_bgra_opaque(src: &[u8], dst: &mut [u8]) {
    assert_eq!(src.len(), dst.len(), "幕布像素长度不一致");
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
        d[3] = 255;
    }
}

/// 建窗（主线程调用）。隐藏 + 不抢焦点 + 不进任务栏；永不接收输入
/// （webview 盖在上面，全部交互由它处理）。
unsafe fn create_window() -> Result<BackdropWindow, String> {
    static REGISTERED: OnceCell<()> = OnceCell::new();
    let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW 失败: {e}"))?;
    // CreateWindowExW 收 HINSTANCE；Win32 里两者同为进程基址句柄。
    let hinstance = windows::Win32::Foundation::HINSTANCE(hmodule.0);
    // 类名缓冲必须活过 RegisterClassW + CreateWindowExW 两个调用。
    let class_name = wide(CLASS_NAME);
    let class_ptr = PCWSTR::from_raw(class_name.as_ptr());
    REGISTERED.get_or_init(|| {
        let wc = WNDCLASSW {
            style: Default::default(),
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_ptr,
        };
        // 重复注册报错无害（类已存在），忽略。
        let _ = RegisterClassW(&wc);
    });
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class_ptr,
        PCWSTR::null(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    )
    .map_err(|e| format!("幕布窗创建失败: {e}"))?;
    let _ = ShowWindow(hwnd, SW_HIDE);
    Ok(BackdropWindow {
        hwnd,
        dc_mem: HDC::default(),
        dib: HBITMAP::default(),
        bits: std::ptr::null_mut(),
        width: 0,
        height: 0,
    })
}

/// UTF-8 → NUL 结尾 UTF-16（windows 0.58 没有 w! 宏，自带一个最小的）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 幕布窗无交互、无自绘消息（ULW 直贴），默认过程足够。
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::copy_bgra_opaque;

    /// alpha 强制 255，BGR 通道原样保留（ULW 预乘语义下 255 恒等）。
    #[test]
    fn copy_forces_opaque_alpha_and_keeps_channels() {
        // GDI 抓屏的 alpha 字节不可靠：故意给 0 / 垃圾值。
        let src = vec![1, 2, 3, 0, 4, 5, 6, 77];
        let mut dst = vec![0u8; src.len()];
        copy_bgra_opaque(&src, &mut dst);
        assert_eq!(dst, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
