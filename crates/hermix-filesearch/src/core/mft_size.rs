//! 从 MFT file record 里读文件体积（`$DATA` 属性）。
//!
//! **为什么 USN 不够**：USN 记录（`FSCTL_ENUM_USN_DATA`）只有 FRN、父 FRN、
//! 名字、属性位——**没有大小**。要体积必须读 file record 本身。
//!
//! **与 WizTree 的差别**：WizTree 自己解析 NTFS 内部结构、批量读 `$MFT` 的
//! data runs（1TB 盘 5 秒）；我们是"一次 IOCTL 一个文件"（`FSCTL_GET_NTFS_
//! FILE_RECORD`），多线程下百万文件约 1–3 秒。代价与收益的取舍是明确的：
//! 不碰 NTFS 内部结构（那段代码易错且随版本变），换来的是可维护性。
//!
//! **取哪个大小**：非驻留属性的 `RealSize`（文件真实字节数，稀疏/压缩文件
//! 会小于占盘体积）；目录没有 `$DATA`，体积由上层按父子关系累加。

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

/// FILE_NTFS = 3（`FILE_ID_DESCRIPTOR.FileSystem`）。
const FILE_NTFS: u32 = 3;

/// `FSCTL_GET_NTFS_FILE_RECORD` =
/// `CTL_CODE(FILE_FILE_SYSTEM, 63, METHOD_BUFFERED, FILE_ANY_ACCESS)`
/// = (9 << 16) | (63 << 2) = 0x000900FC。
const FSCTL_GET_NTFS_FILE_RECORD: u32 = 0x0009_00FC;

/// FSCTL 的输入结构（`FILE_ID_DESCRIPTOR`）。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FileIdDescriptor {
    file_system: u32,
    padding: u32,
    file_reference_number: u64,
    file_index: u64,
}

/// 读到的文件体积。
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSize {
    /// 文件真实字节数（稀疏/压缩文件小于占盘体积）。
    pub bytes: u64,
    /// 占盘字节数（分配大小）。
    pub allocated_bytes: u64,
    /// 记录里没有 `$DATA`（目录 / 特殊文件）。
    pub is_dir: bool,
}

/// 一次 IOCTL 取回的 record 缓冲区大小。MFT 记录最大 1KB（4KB 足够）。
const RECORD_BUFFER: usize = 4096;

/// 批量读体积：多线程，每线程一个卷句柄。
///
/// 失败一律降级（返回空 map），绝不因为拿不到体积就让整个索引失败——
/// 体积只影响"磁盘占用"这个视图，不影响搜索。
pub fn read_file_sizes(volume: &str, file_refs: &[u64], threads: usize) -> HashMap<u64, FileSize> {
    let mut out = HashMap::new();
    if file_refs.is_empty() {
        return out;
    }
    let workers = threads.clamp(1, 16);
    let chunk = file_refs.len().div_ceil(workers);

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for part in file_refs.chunks(chunk) {
            let volume = volume.to_string();
            handles.push(scope.spawn(move || {
                let Ok(handle) = open_volume(&volume) else {
                    return Vec::new();
                };
                let mut local = Vec::with_capacity(part.len());
                for file_ref in part {
                    if let Some(size) = read_one(handle, *file_ref) {
                        local.push((*file_ref, size));
                    }
                }
                local
            }));
        }
        for handle in handles {
            if let Ok(pairs) = handle.join() {
                out.extend(pairs);
            }
        }
    });

    out
}

/// 单个 FRN → 体积。句柄是 `\\.\C:` 形式的卷句柄。
fn read_one(handle: std::os::windows::raw::HANDLE, file_ref: u64) -> Option<FileSize> {
    use std::mem::size_of;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let descriptor = FileIdDescriptor {
        file_system: FILE_NTFS,
        padding: 0,
        file_reference_number: file_ref,
        file_index: 0,
    };
    let mut buffer = vec![0u8; RECORD_BUFFER];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_FILE_RECORD,
            &descriptor as *const _ as *const _,
            size_of::<FileIdDescriptor>() as u32,
            buffer.as_mut_ptr() as *mut _,
            buffer.len() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        let _ = unsafe { GetLastError() };
        return None;
    }
    let used = (returned as usize).min(buffer.len());
    if used == 0 {
        return None;
    }
    parse_record(&buffer[..used])
}

/// 打开卷句柄（`\\.\C:`）。MFT 读取需要管理员权限。
fn open_volume(volume: &str) -> Result<std::os::windows::raw::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let path = format!(r"\\.\{}", volume.trim_end_matches('\\'));
    let wide: Vec<u16> = std::ffi::OsStr::new(&path)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        anyhow::bail!("打开卷 {volume} 失败（需要管理员权限）");
    }
    Ok(handle)
}

// ── FILE record 解析 ───────────────────────────────────────────────────

/// 从 MFT record 里取未命名 `$DATA` 的大小。
pub fn parse_record(buffer: &[u8]) -> Option<FileSize> {
    if buffer.len() < 48 || &buffer[0..4] != b"FILE" {
        return None;
    }

    // 记录可能已被内核修好（fixup 已应用），也可能还是磁盘原样。
    // 两种都要能解析：先试修，修不上就按原样读。
    let fixed = apply_fixups(buffer);

    // AttrOffset @0x12；Flags @0x14（bit1 = 目录）。
    let attr_offset = u16::from_le_bytes([fixed[0x12], fixed[0x13]]) as usize;
    let flags = u16::from_le_bytes([fixed[0x14], fixed[0x15]]);
    let is_dir = flags & 0x02 != 0;

    let mut cursor = attr_offset;
    let mut result = FileSize {
        bytes: 0,
        allocated_bytes: 0,
        is_dir,
    };
    let mut found = false;

    while cursor + 8 <= fixed.len() {
        let attr_type = u32::from_le_bytes([
            fixed[cursor],
            fixed[cursor + 1],
            fixed[cursor + 2],
            fixed[cursor + 3],
        ]);
        let attr_len = u32::from_le_bytes([
            fixed[cursor + 4],
            fixed[cursor + 5],
            fixed[cursor + 6],
            fixed[cursor + 7],
        ]) as usize;
        if attr_len < 16 || cursor + attr_len > fixed.len() {
            break;
        }
        if attr_type == 0xFFFF {
            // 结束标记。
            break;
        }
        let non_resident = fixed[cursor + 8] != 0;
        let name_length = fixed[cursor + 9] as usize;

        if attr_type == 0x80 && name_length == 0 {
            // `$DATA`（0x80）且无名字 = 文件内容流。
            if non_resident {
                if cursor + 0x28 <= fixed.len() {
                    let allocated = u64::from_le_bytes(
                        fixed[cursor + 0x18..cursor + 0x20].try_into().ok()?,
                    );
                    let real = u64::from_le_bytes(fixed[cursor + 0x20..cursor + 0x28].try_into().ok()?);
                    result.allocated_bytes = allocated;
                    result.bytes = real;
                }
            } else if cursor + 0x14 <= fixed.len() {
                // 驻留：内容在记录里，长度就是文件大小。
                let value_len = u32::from_le_bytes(
                    fixed[cursor + 0x10..cursor + 0x14].try_into().ok()?,
                ) as u64;
                result.bytes = value_len;
                result.allocated_bytes = value_len;
            }
            found = true;
            break;
        }

        cursor += attr_len;
    }

    if found {
        Some(result)
    } else {
        // 目录 / 无 $DATA 的条目：给个"是目录"的结论，体积交上层累加。
        Some(FileSize {
            bytes: 0,
            allocated_bytes: 0,
            is_dir,
        })
    }
}

/// 应用更新序列（fixup）：每个扇区末尾 2 字节被序列号覆盖。
///
/// 已经是修好的记录（内核给的）修不上——此时原值与序列不匹配，直接放弃
/// 修整，按原样解析。两种来源都能读。
fn apply_fixups(buffer: &[u8]) -> Vec<u8> {
    let mut record = buffer.to_vec();
    if record.len() < 8 {
        return record;
    }
    let usa_offset = u16::from_le_bytes([record[4], record[5]]) as usize;
    let usa_count = u16::from_le_bytes([record[6], record[7]]) as usize;
    if usa_count < 2 || usa_offset + usa_count * 2 > record.len() {
        return record;
    }
    let usn = u16::from_le_bytes([record[usa_offset], record[usa_offset + 1]]);
    for i in 1..usa_count {
        let us = usa_offset + i * 2;
        if us + 2 > record.len() {
            break;
        }
        let replacement = u16::from_le_bytes([record[us], record[us + 1]]);
        let tail = i * 512 - 2;
        if tail + 2 > record.len() {
            break;
        }
        let current = u16::from_le_bytes([record[tail], record[tail + 1]]);
        if current != usn {
            // 已经是修好的记录（或损坏）：不动它。
            return buffer.to_vec();
        }
        record[tail] = replacement as u8;
        record[tail + 1] = (replacement >> 8) as u8;
    }
    record
}

/// 探测当前进程能不能读 MFT（用于给 UI 明确提示，而不是报一句"没数据"）。
pub fn mft_readable(volume: &str) -> bool {
    let Ok(handle) = open_volume(volume) else {
        return false;
    };
    use windows_sys::Win32::Foundation::CloseHandle;
    unsafe {
        let _ = CloseHandle(handle);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条最小可用的 MFT record：非驻留 `$DATA`，real size 12345。
    fn synth_record(real_size: u64, allocated: u64, is_dir: bool) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        buf[0..4].copy_from_slice(b"FILE");
        buf[4..6].copy_from_slice(&0x30u16.to_le_bytes()); // usa offset
        buf[6..8].copy_from_slice(&1u16.to_le_bytes()); // usa count = 1（无 fixup）
        buf[0x12..0x14].copy_from_slice(&0x38u16.to_le_bytes()); // attr offset
        buf[0x14..0x16].copy_from_slice(&(if is_dir { 0x02u16 } else { 0x01u16 }).to_le_bytes());

        // $DATA 属性（非驻留）
        let attr = 0x38usize;
        buf[attr..attr + 4].copy_from_slice(&0x80u32.to_le_bytes()); // type = $DATA
        buf[attr + 4..attr + 8].copy_from_slice(&0x48u32.to_le_bytes()); // length
        buf[attr + 8] = 1; // non resident
        buf[attr + 9] = 0; // 无名字
        buf[attr + 0x18..attr + 0x20].copy_from_slice(&allocated.to_le_bytes());
        buf[attr + 0x20..attr + 0x28].copy_from_slice(&real_size.to_le_bytes());
        buf
    }

    #[test]
    fn parses_non_resident_data_size() {
        let buf = synth_record(12345, 16384, false);
        let size = parse_record(&buf).expect("parsed");
        assert_eq!(size.bytes, 12345);
        assert_eq!(size.allocated_bytes, 16384);
        assert!(!size.is_dir);
    }

    #[test]
    fn marks_directory_entries() {
        let buf = synth_record(0, 0, true);
        let size = parse_record(&buf).expect("parsed");
        assert!(size.is_dir);
        assert_eq!(size.bytes, 0);
    }

    #[test]
    fn rejects_non_file_buffers() {
        assert!(parse_record(b"not a file record at all").is_none());
        assert!(parse_record(&[0u8; 8]).is_none());
    }

    /// 驻留 $DATA（小于 700 字节的文件）：大小在 value length 里。
    #[test]
    fn parses_resident_data_size() {
        let mut buf = vec![0u8; 512];
        buf[0..4].copy_from_slice(b"FILE");
        buf[6..8].copy_from_slice(&1u16.to_le_bytes());
        buf[0x12..0x14].copy_from_slice(&0x38u16.to_le_bytes());
        let attr = 0x38usize;
        buf[attr..attr + 4].copy_from_slice(&0x80u32.to_le_bytes());
        buf[attr + 4..attr + 8].copy_from_slice(&0x40u32.to_le_bytes());
        buf[attr + 8] = 0; // resident
        buf[attr + 9] = 0;
        buf[attr + 0x10..attr + 0x14].copy_from_slice(&321u32.to_le_bytes());
        let size = parse_record(&buf).expect("parsed");
        assert_eq!(size.bytes, 321);
    }

    /// 修整后的记录（内核已应用 fixup）也要能解析：修不上就按原样读。
    #[test]
    fn tolerates_already_fixed_records() {
        let mut buf = synth_record(999, 1024, false);
        // 伪造一个"看起来该被修"的扇区尾：与 USN 不一致 → 放弃修整。
        buf[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        buf[6..8].copy_from_slice(&3u16.to_le_bytes());
        buf[0x30..0x32].copy_from_slice(&0x1234u16.to_le_bytes());
        buf[0x32..0x34].copy_from_slice(&0x5678u16.to_le_bytes());
        let size = parse_record(&buf).expect("parsed");
        assert_eq!(size.bytes, 999);
    }
}
