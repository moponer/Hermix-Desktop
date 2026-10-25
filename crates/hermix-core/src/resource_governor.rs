//! 资源治理原语（总管方案 P0）：目录体积统计、scratch 目录配额、
//! 磁盘剩余空间水位。
//!
//! Windows 没有 per-process 磁盘配额（FSRM 仅 Server SKU，NTFS 配额按用户
//! 不按进程），磁盘治理走组合拳：worker 写入白名单目录（scratch）→ host
//! 侧周期统计体积 → 超配额停派；另设整机水位（尤其 C 盘），低于阈值全局
//! 停派 + 告警。本模块只提供纯同步原语；周期巡检与停派策略在 server 侧
//! 接线（成员会话 / worker 池落地时）。
//!
//! 体积统计是 best-effort 遍历：条目不可读（权限/占用）时跳过并继续；
//! 重解析点（junction/symlink，`FILE_ATTRIBUTE_REPARSE_POINT`）不跟随——
//! scratch 里若有 junction，跟随会导致重复计数甚至环。大目录遍历是
//! O(文件数)，配额的意义正在于让 scratch 目录不会长到不可遍历。

#![cfg(windows)]

use std::io;
use std::path::Path;

type Bool = i32;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// C 盘默认低水位：剩余空间低于该值即全局停派（总管方案 D5）。
pub const DEFAULT_C_DRIVE_MIN_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// 单 worker scratch 目录默认配额（总管方案 D5）。
pub const DEFAULT_SCRATCH_CAP_BYTES: u64 = 5 * 1024 * 1024 * 1024;

extern "system" {
    fn GetDiskFreeSpaceExW(
        directory_name: *const u16,
        free_bytes_available_to_caller: *mut u64,
        total_number_of_bytes: *mut u64,
        total_number_of_free_bytes: *mut u64,
    ) -> Bool;
}

/// 卷剩余可用空间（字节）。`root` 只要是该卷上的任意路径即可。
pub fn disk_free_bytes(root: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut avail = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

/// 目录体积（字节）：递归遍历文件长度求和。目录不存在/不可读返回 0。
/// 重解析点不跟随（见模块文档）。
pub fn dir_size_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            // `DirEntry::metadata` 在 Windows 上不穿透重解析点（等价
            // symlink_metadata），拿到的正是条目自身的属性。
            let Ok(md) = entry.metadata() else { continue };
            use std::os::windows::fs::MetadataExt;
            if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                continue;
            }
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                total += md.len();
            }
        }
    }
    total
}

/// scratch 目录配额判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaVerdict {
    Within,
    Exceeded { used_bytes: u64, cap_bytes: u64 },
}

/// 统计 `dir` 体积并对照 `cap_bytes` 判定配额。
pub fn check_dir_quota(dir: &Path, cap_bytes: u64) -> QuotaVerdict {
    let used = dir_size_bytes(dir);
    if used > cap_bytes {
        QuotaVerdict::Exceeded { used_bytes: used, cap_bytes }
    } else {
        QuotaVerdict::Within
    }
}

/// 磁盘水位判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatermarkVerdict {
    Healthy,
    /// 剩余空间低于阈值：应停派新任务并告警。
    Low { free_bytes: u64, min_free_bytes: u64 },
}

/// 检查 `root` 所在卷的剩余空间是否低于 `min_free_bytes`。
pub fn check_watermark(root: &Path, min_free_bytes: u64) -> io::Result<WatermarkVerdict> {
    let free = disk_free_bytes(root)?;
    Ok(if free < min_free_bytes {
        WatermarkVerdict::Low { free_bytes: free, min_free_bytes }
    } else {
        WatermarkVerdict::Healthy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dir_size_sums_nested_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("a.bin"), vec![0u8; 1024]).expect("write a");
        fs::create_dir(root.join("sub")).expect("mkdir");
        fs::write(root.join("sub").join("b.bin"), vec![0u8; 2048]).expect("write b");
        assert_eq!(dir_size_bytes(root), 3072);
    }

    #[test]
    fn dir_size_missing_dir_is_zero() {
        assert_eq!(dir_size_bytes(Path::new("Z:/definitely/not/here")), 0);
    }

    #[test]
    fn quota_verdict_thresholds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("x.bin"), vec![0u8; 100]).expect("write");
        assert_eq!(check_dir_quota(tmp.path(), 100), QuotaVerdict::Within);
        assert_eq!(
            check_dir_quota(tmp.path(), 99),
            QuotaVerdict::Exceeded { used_bytes: 100, cap_bytes: 99 }
        );
    }

    #[test]
    fn disk_free_bytes_on_temp_dir_is_positive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let free = disk_free_bytes(tmp.path()).expect("free bytes");
        assert!(free > 0, "临时目录所在卷剩余空间应当为正，得到 {free}");
    }

    #[test]
    fn watermark_verdict_thresholds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let healthy = check_watermark(tmp.path(), 0).expect("check");
        assert_eq!(healthy, WatermarkVerdict::Healthy);
        let low = check_watermark(tmp.path(), u64::MAX).expect("check");
        assert!(matches!(low, WatermarkVerdict::Low { .. }));
    }
}
