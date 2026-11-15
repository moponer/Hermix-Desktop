//! 剪贴板里的**文件**（CF_HDROP）——资源管理器式「复制/粘贴文件」。
//!
//! 为什么必须走 Win32：浏览器 Clipboard API 只能写文本与图片，写不了
//! 「文件列表」。资源管理器里 Ctrl+C 复制的是 CF_HDROP 格式，别的程序
//! （Word / VS Code / 终端 / 另一个资源管理器窗口）才能接上。viewer's
//! 右键菜单要提供「复制文件」「粘贴文件」，就只能自己拼 DROPFILES。
//!
//! 字节布局（x64，20 字节头 + UTF-16 路径表 + 双 NUL 结尾）：
//! ```text
//!   DROPFILES { pFiles, pt, fNC, fWide }  |  "C:\a.txt\0" "C:\b\0" \0
//!   ^ pFiles = 20（路径表相对头部的偏移）
//! ```
//! `fWide = TRUE` 表示路径是 UTF-16。
//!
//! 纯字节拼装部分（`dropfiles_payload`）不碰 Win32，可单测。

use std::path::{Path, PathBuf};

/// DROPFILES 头 + UTF-16 路径表（每条以 NUL 结尾，表尾再一个 NUL）。
///
/// 纯函数：布局错了剪贴板里就是空文件，Win32 侧只会返回"0 个文件"，
/// 很难看出是哪一步错——所以单独拆出来测字节。
pub fn dropfiles_payload(paths: &[String]) -> Vec<u8> {
    #[cfg(windows)]
    let header_size = {
        use windows_sys::Win32::UI::Shell::DROPFILES;
        std::mem::size_of::<DROPFILES>()
    };
    #[cfg(not(windows))]
    let header_size = 20usize;

    // 路径表：每条 UTF-16 + NUL，表尾再来一个 NUL（成对出现）。
    let mut list: Vec<u8> = Vec::new();
    for p in paths {
        for unit in p.encode_utf16() {
            list.extend_from_slice(&unit.to_le_bytes());
        }
        list.extend_from_slice(&0u16.to_le_bytes());
    }
    list.extend_from_slice(&0u16.to_le_bytes());

    let mut out: Vec<u8> = Vec::with_capacity(header_size + list.len());
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::DROPFILES;
        // packed(1)：不能取字段引用（E0793），只能按字节搬。
        let mut hdr: DROPFILES = unsafe { std::mem::zeroed() };
        hdr.pFiles = header_size as u32;
        hdr.fWide = 1; // TRUE：路径是 UTF-16
        out.extend_from_slice(unsafe {
            std::slice::from_raw_parts(&hdr as *const DROPFILES as *const u8, header_size)
        });
    }
    #[cfg(not(windows))]
    {
        out.extend_from_slice(&(header_size as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 12]);
    }
    out.extend_from_slice(&list);
    out
}

/// 打开剪贴板并执行 f，结束后关闭。
///
/// OpenClipboard 会因为别的进程正持有剪贴板而失败（几毫秒级），
/// 所以重试几次而不是直接报错。
#[cfg(windows)]
fn with_clipboard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};
    let mut opened = false;
    for _ in 0..12 {
        // HWND 在 windows-sys 0.52 里是 isize：0 = 无 owner（剪贴板不绑定窗口）。
        if unsafe { OpenClipboard(0) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    if !opened {
        return Err("无法访问剪贴板（可能被其他程序占用）".into());
    }
    let r = f();
    unsafe {
        CloseClipboard();
    }
    r
}

/// 把文件/文件夹复制到剪贴板（CF_HDROP）。返回写入的条目数。
#[cfg(windows)]
pub fn copy_to_clipboard(paths: &[String]) -> Result<usize, String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };
    use windows_sys::Win32::System::Ole::CF_HDROP;

    if paths.is_empty() {
        return Err("没有要复制的项目".into());
    }
    // 只放真实存在的路径：剪贴板里躺着不存在的文件，粘贴时会被 Explorer
    // 静默忽略，用户会以为「复制没生效」。
    let mut existing: Vec<String> = Vec::new();
    for p in paths {
        if Path::new(p).exists() {
            existing.push(p.clone());
        }
    }
    if existing.is_empty() {
        return Err("要复制的项目不存在".into());
    }

    let payload = dropfiles_payload(&existing);
    // 剪贴板内存必须是 GMEM_MOVEABLE：SetClipboardData 接管后由系统释放，
    // 用可移动内存才能被系统重新映射。
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, payload.len()) };
    if hmem.is_null() {
        return Err("内存分配失败".into());
    }
    unsafe {
        let dst = GlobalLock(hmem);
        if dst.is_null() {
            GlobalFree(hmem);
            return Err("内存加锁失败".into());
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), dst as *mut u8, payload.len());
        GlobalUnlock(hmem);
    }

    with_clipboard(|| {
        unsafe {
            EmptyClipboard();
        }
        let ok = unsafe { SetClipboardData(CF_HDROP as u32, hmem as isize) };
        if ok == 0 {
            // 没交出去 = 内存还是我们的，必须自己释放（否则泄漏）。
            unsafe {
                GlobalFree(hmem);
            }
            return Err("写入剪贴板失败".into());
        }
        Ok(existing.len())
    })
}

/// 读剪贴板里的文件列表（CF_HDROP）。剪贴板里没有文件时返回空数组。
#[cfg(windows)]
pub fn paste_from_clipboard() -> Result<Vec<PathBuf>, String> {
    use windows_sys::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::System::Ole::CF_HDROP;
    use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

    with_clipboard(|| {
        if unsafe { IsClipboardFormatAvailable(CF_HDROP as u32) } == 0 {
            return Ok(Vec::new());
        }
        let handle = unsafe { GetClipboardData(CF_HDROP as u32) };
        if handle == 0 {
            return Err("读取剪贴板失败".into());
        }
        let hdrop = handle as HDROP;
        // 0xFFFFFFFF = 只问条数。
        let count = unsafe { DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0) };
        let mut out: Vec<PathBuf> = Vec::new();
        for i in 0..count {
            let need = unsafe { DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0) } + 1;
            let mut buf = vec![0u16; need as usize];
            let got = unsafe { DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32) };
            if got == 0 {
                continue;
            }
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got as usize])));
        }
        Ok(out)
    })
}

/// 写文本到剪贴板（CF_UNICODETEXT）。
///
/// 不用浏览器 Clipboard API：WebView2 里 `navigator.clipboard.readText()`
/// 要 clipboard-read 权限，而 WebView2 没有权限弹窗——直接失败。
/// 走 Win32 没有权限这回事。
#[cfg(windows)]
pub fn write_text(text: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let bytes = buf.len() * 2;
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
    if hmem.is_null() {
        return Err("内存分配失败".into());
    }
    unsafe {
        let dst = GlobalLock(hmem);
        if dst.is_null() {
            GlobalFree(hmem);
            return Err("内存加锁失败".into());
        }
        std::ptr::copy_nonoverlapping(buf.as_ptr() as *const u8, dst as *mut u8, bytes);
        GlobalUnlock(hmem);
    }
    with_clipboard(|| {
        unsafe {
            EmptyClipboard();
        }
        let ok = unsafe { SetClipboardData(CF_UNICODETEXT as u32, hmem as isize) };
        if ok == 0 {
            unsafe {
                GlobalFree(hmem);
            }
            return Err("写入剪贴板失败".into());
        }
        Ok(())
    })
}

/// 读剪贴板文本（CF_UNICODETEXT）。
#[cfg(windows)]
pub fn read_text() -> Result<String, String> {
    use windows_sys::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    with_clipboard(|| {
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
            return Err("剪贴板里没有文本".into());
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        if handle == 0 {
            return Err("读取剪贴板失败".into());
        }
        let ptr = handle as *const u16;
        if ptr.is_null() {
            return Err("读取剪贴板失败".into());
        }
        // 剪贴板里的文本是 NUL 结尾的 UTF-16；必须在剪贴板打开期间读。
        unsafe {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            Ok(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)))
        }
    })
}

/// 递归复制：文件用 copy，目录逐项递归。
///
/// 目标已存在时**不覆盖**——粘贴是"新增"语义，静默覆盖用户文件是不可
/// 逆的数据丢失，Explorer 也会弹窗问，而弹窗在这里是额外依赖。
pub fn copy_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("无法访问 {}: {e}", src.display()))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("无法创建 {}: {e}", dst.display()))?;
        for item in std::fs::read_dir(src).map_err(|e| format!("无法读取目录: {e}"))? {
            let item = item.map_err(|e| format!("目录项读取失败: {e}"))?;
            let name = item.file_name();
            copy_recursive(&item.path(), &dst.join(name))?;
        }
        Ok(())
    } else {
        if dst.exists() {
            return Err(format!("目标已存在: {}", dst.display()));
        }
        std::fs::copy(src, dst).map_err(|e| format!("复制失败 {}: {e}", src.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::dropfiles_payload;

    /// DROPFILES 头必须在路径表之前，且 pFiles 指向路径表起点——
    /// 偏移错了 Explorer 收到的是一串乱码路径。
    #[test]
    fn dropfiles_layout_is_correct() {
        let payload = dropfiles_payload(&["C:\\a.txt".to_string(), "C:\\b".to_string()]);
        let pfiles = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        assert_eq!(pfiles as usize, 20, "路径表偏移必须是头部长度");
        // 路径表：两条 UTF-16 路径，各带 NUL 结尾，末尾再一个 NUL。
        let tail = &payload[pfiles as usize..];
        let units: Vec<u16> = tail
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut expect: Vec<u16> = Vec::new();
        for p in ["C:\\a.txt", "C:\\b"] {
            expect.extend(p.encode_utf16());
            expect.push(0);
        }
        expect.push(0);
        assert_eq!(units, expect);
    }

    /// 空列表也要能拼出合法载荷（表尾双 NUL）——否则调用方会拿到
    /// 一个没有终止符的路径表。
    #[test]
    fn dropfiles_empty_list_is_terminated() {
        let payload = dropfiles_payload(&[]);
        assert_eq!(payload.len(), 22, "20 字节头 + 双 NUL");
        assert_eq!(&payload[20..], &[0, 0]);
    }
}
