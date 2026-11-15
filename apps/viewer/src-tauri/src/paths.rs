//! 产物路径守卫：图片 / PDF / 文本三条写盘路径**共用一份**实现。
//!
//! 为什么要抽出来：两份独立实现会漂——2026-10-20 的独立审查正是发现
//! 「修了图片侧、PDF 侧漏修」这类问题才把它逼出来的。
//!
//! **判据不能只有路径**：NTFS 硬链接下同一个文件有多个路径，
//! 写 `b.pdf` 会截断 `a.pdf` 的内容 → 原文件被销毁。实测（2026-10-20）：
//! 两个路径 `canonicalize` 后不同（守卫放行），但 volume serial +
//! file index 完全相同（同一个 inode）。所以最后一步必须比**文件身份**。
//!
//! 三层判据（任一命中即「同一文件」）：
//!   1. 规范化绝对路径 + 小写（Windows 路径大小写不敏感）
//!   2. `canonicalize`（覆盖 `..`、相对路径、符号链接）
//!   3. `GetFileInformationByHandle` 的 volume serial + file index（抓硬链接）

use std::path::{Path, PathBuf};

/// 默认产物名：`<原名>.edited.<扩展名>`（同目录）。
///
/// `ext` 是**目标格式**的扩展名——格式转换时必须跟着格式走，否则会出现
/// 「png 转 webp，产物却叫 .edited.png」这种名实不符的文件。
pub fn default_output_path(src: &Path, ext: Option<&str>) -> PathBuf {
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string());
    let ext = ext
        .map(str::to_string)
        .or_else(|| src.extension().map(|s| s.to_string_lossy().to_string()));
    let dir = src.parent().unwrap_or_else(|| Path::new(""));
    let name = match (stem, ext) {
        (Some(s), Some(e)) => format!("{s}.edited.{e}"),
        (Some(s), None) => format!("{s}.edited"),
        _ => "edited".to_string(),
    };
    dir.join(name)
}

/// 同一个文件吗？三层判据见模块文档。
pub fn same_file(a: &Path, b: &Path) -> bool {
    // 1) 规范化绝对路径 + 小写
    let norm = |p: &Path| {
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        };
        abs.to_string_lossy().to_lowercase()
    };
    if norm(a) == norm(b) {
        return true;
    }
    // 2) canonicalize：覆盖 .. / 相对路径 / 符号链接
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => {
            if x == y {
                return true;
            }
            // 3) 文件身份：抓硬链接
            same_identity(&x, &y)
        }
        _ => false,
    }
}

/// 产物路径守卫：与源文件相同一律拒绝（哪怕前端传了同一个路径）。
///
/// `ext` = 目标格式的扩展名（None = 沿用源文件扩展名）。
pub fn safe_output_path(
    src: &Path,
    requested: Option<&str>,
    ext: Option<&str>,
) -> Result<PathBuf, String> {
    let candidate = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_output_path(src, ext),
    };
    if same_file(&candidate, src) {
        return Err("产物路径不能与原文件相同：原文件只读，请换一个文件名".into());
    }
    Ok(candidate)
}

/// Windows：比 volume serial + file index（同一个 inode 的硬链接）。
#[cfg(windows)]
fn same_identity(a: &Path, b: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    fn identity(path: &Path) -> Option<(u32, u64)> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // 访问权限给 0：只要拿到句柄读元信息，不打开文件内容。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) } != 0;
        unsafe {
            CloseHandle(handle);
        }
        if !ok {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
        Some((info.dwVolumeSerialNumber, index))
    }

    match (identity(a), identity(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 非 Windows：没有硬链接判据，退回路径判据（项目 Windows-only，
/// 这条分支只为编译通过）。
#[cfg(not(windows))]
fn same_identity(_a: &Path, _b: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_path_follows_target_format() {
        let p = default_output_path(Path::new(r"C:\pics\cat.png"), Some("webp"));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cat.edited.webp");
        let p = default_output_path(Path::new(r"C:\pics\cat.png"), None);
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cat.edited.png");
    }

    #[test]
    fn safe_output_path_rejects_source_and_case_variant() {
        let src = PathBuf::from(r"C:\pics\cat.png");
        assert!(safe_output_path(&src, Some(r"C:\pics\cat.png"), None)
            .unwrap_err()
            .contains("不能与原文件相同"));
        assert!(safe_output_path(&src, Some(r"C:\Pics\CAT.PNG"), None)
            .unwrap_err()
            .contains("不能与原文件相同"));
    }

    /// 硬链接：两个路径不同、同一个 inode —— 守卫必须拒。
    #[cfg(windows)]
    #[test]
    fn safe_output_path_rejects_hardlink() {
        let dir = std::env::temp_dir().join(format!("hermix-viewer-hl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        std::fs::write(&a, b"hello").unwrap();
        let b = dir.join("b.bin");
        // mklink /H 需要管理员或开发者模式；建不出来就跳过（不算失败）。
        let linked = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/H", b.to_str().unwrap(), a.to_str().unwrap()])
            .output();
        match linked {
            Ok(o) if o.status.success() => {
                assert!(
                    same_file(&a, &b),
                    "硬链接应判定为同一文件（否则守卫会放行覆盖原文件）"
                );
                assert!(safe_output_path(&a, Some(b.to_str().unwrap()), None).is_err());
            }
            _ => {
                // 没建出硬链接：至少确认同路径仍然被拒。
                assert!(safe_output_path(&a, Some(a.to_str().unwrap()), None).is_err());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
