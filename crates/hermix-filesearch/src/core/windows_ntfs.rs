use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::{
    index::build_entries_from_records,
    model::{BuildIndexSummary, FileEntry, FilterRules, NtfsRecord, VolumeIndexState, VolumeInfo},
};

pub struct NtfsIndexBuild {
    pub state: VolumeIndexState,
    pub entries: Vec<FileEntry>,
    pub summary: BuildIndexSummary,
}

pub struct UsnChanges {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub records: Vec<NtfsRecord>,
}

#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsStr,
        mem::size_of,
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
        slice,
    };

    use anyhow::{bail, Result};
    use windows_sys::{
        core::PWSTR,
        Wdk::Storage::FileSystem::{
            MFT_ENUM_DATA, READ_USN_JOURNAL_DATA, USN_JOURNAL_DATA, USN_RECORD,
        },
        Win32::{
            Foundation::{
                CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF,
                ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE, HANDLE,
                INVALID_HANDLE_VALUE,
            },
            Storage::FileSystem::{
                CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
                FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ,
                FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            },
            System::{
                Ioctl::{FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL},
                WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
                IO::DeviceIoControl,
            },
        },
    };

    use super::{
        build_entries_from_records, now_ms, BuildIndexSummary, FilterRules, Instant, NtfsIndexBuild,
        NtfsRecord, UsnChanges, VolumeIndexState, VolumeInfo,
    };

    use crate::core::metadata::hydrate_file_metadata;
    const BUFFER_SIZE: usize = 1024 * 1024;

    pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
        let mask = unsafe { GetLogicalDrives() };
        let mut volumes = Vec::new();

        for index in 0..26 {
            if mask & (1 << index) == 0 {
                continue;
            }

            let letter = (b'A' + index as u8) as char;
            let name = format!("{letter}:");
            let root = format!("{name}\\");
            let root_wide = wide_null(&root);
            let drive_type_code = unsafe { GetDriveTypeW(root_wide.as_ptr()) };
            let drive_type = drive_type_label(drive_type_code).to_string();

            let (label, file_system) =
                volume_information(&root).unwrap_or_else(|_| (String::new(), String::new()));
            let is_local = drive_type_code == DRIVE_FIXED;
            let is_ntfs = file_system.eq_ignore_ascii_case("NTFS");
            let is_supported = is_local && is_ntfs;
            let reason = if is_supported {
                None
            } else if !is_local {
                Some("仅支持本地固定磁盘".to_string())
            } else if file_system.is_empty() {
                Some("无法读取文件系统类型".to_string())
            } else {
                Some(format!("暂不支持 {file_system} 文件系统"))
            };

            volumes.push(VolumeInfo {
                name: name.clone(),
                display_name: root,
                label,
                file_system,
                drive_type,
                is_supported,
                reason,
            });
        }

        Ok(volumes)
    }

    pub fn build_volume_index(volume: String, filter: &FilterRules) -> Result<NtfsIndexBuild> {
        let volume = normalize_volume(&volume)?;
        ensure_supported_volume(&volume)?;
        let start = Instant::now();
        let handle = VolumeHandle::open(&volume)?;
        let journal = query_usn_journal(handle.raw())?;
        let records = enum_mft_records(handle.raw(), journal.NextUsn)?;
        let mut entries = build_entries_from_records(&volume, records, filter);
        hydrate_file_metadata(&mut entries);
        let files = entries.iter().filter(|entry| !entry.is_dir).count();
        let directories = entries.iter().filter(|entry| entry.is_dir).count();
        let now = now_ms();
        let state = VolumeIndexState {
            volume: volume.clone(),
            journal_id: journal.UsnJournalID,
            first_usn: journal.FirstUsn,
            next_usn: journal.NextUsn,
            indexed_at: now,
            last_synced_at: now,
            source: "MFT".to_string(),
        };
        let summary = BuildIndexSummary {
            volume,
            total_entries: entries.len(),
            files,
            directories,
            elapsed_ms: start.elapsed().as_millis() as u64,
            journal_id: state.journal_id,
            next_usn: state.next_usn,
            snapshot_saved: false,
        };

        Ok(NtfsIndexBuild {
            state,
            entries,
            summary,
        })
    }

    pub fn read_usn_changes(state: VolumeIndexState) -> Result<UsnChanges> {
        let handle = VolumeHandle::open(&state.volume)?;
        let journal = query_usn_journal(handle.raw())?;
        let records =
            if journal.UsnJournalID == state.journal_id && journal.FirstUsn <= state.next_usn {
                read_usn_records(
                    handle.raw(),
                    state.next_usn,
                    journal.NextUsn,
                    journal.UsnJournalID,
                )?
            } else {
                Vec::new()
            };

        Ok(UsnChanges {
            journal_id: journal.UsnJournalID,
            first_usn: journal.FirstUsn,
            next_usn: journal.NextUsn,
            records,
        })
    }

    fn ensure_supported_volume(volume: &str) -> Result<()> {
        let root = format!("{volume}\\");
        let root_wide = wide_null(&root);
        let drive_type = unsafe { GetDriveTypeW(root_wide.as_ptr()) };
        if drive_type != DRIVE_FIXED {
            bail!(
                "仅支持本地固定 NTFS 卷，当前卷类型是 {}。",
                drive_type_label(drive_type)
            );
        }

        let (_, file_system) = volume_information(&root)?;
        if !file_system.eq_ignore_ascii_case("NTFS") {
            bail!("当前卷文件系统是 {file_system}，MFT/USN MVP 仅支持 NTFS。");
        }

        Ok(())
    }

    fn volume_information(root: &str) -> Result<(String, String)> {
        let root_wide = wide_null(root);
        let mut label = vec![0u16; 260];
        let mut fs_name = vec![0u16; 32];
        let mut serial = 0u32;
        let mut max_component = 0u32;
        let mut flags = 0u32;

        let ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                label.as_mut_ptr() as PWSTR,
                label.len() as u32,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs_name.as_mut_ptr() as PWSTR,
                fs_name.len() as u32,
            )
        };

        if ok == 0 {
            return Err(last_error("读取卷信息失败"));
        }

        Ok((
            utf16_buffer_to_string(&label),
            utf16_buffer_to_string(&fs_name),
        ))
    }

    fn enum_mft_records(handle: HANDLE, high_usn: i64) -> Result<Vec<NtfsRecord>> {
        let mut input = MFT_ENUM_DATA {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: high_usn,
            MinMajorVersion: 2,
            MaxMajorVersion: 2,
        };
        let mut records = Vec::new();

        loop {
            let mut buffer = vec![0u8; BUFFER_SIZE];
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    &input as *const _ as *const _,
                    size_of::<MFT_ENUM_DATA>() as u32,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
                    &mut returned,
                    null_mut(),
                )
            };

            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_HANDLE_EOF {
                    break;
                }
                return Err(io_error(code, "MFT 枚举失败"));
            }

            if returned as usize <= size_of::<u64>() {
                break;
            }

            let next_file_ref = unsafe { std::ptr::read_unaligned(buffer.as_ptr() as *const u64) };
            parse_records(&buffer[size_of::<u64>()..returned as usize], &mut records)?;
            input.StartFileReferenceNumber = next_file_ref;
        }

        Ok(records)
    }

    fn read_usn_records(
        handle: HANDLE,
        start_usn: i64,
        target_usn: i64,
        journal_id: u64,
    ) -> Result<Vec<NtfsRecord>> {
        let mut input = READ_USN_JOURNAL_DATA {
            StartUsn: start_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 2,
        };
        let mut records = Vec::new();

        loop {
            let mut buffer = vec![0u8; BUFFER_SIZE];
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_READ_USN_JOURNAL,
                    &input as *const _ as *const _,
                    size_of::<READ_USN_JOURNAL_DATA>() as u32,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
                    &mut returned,
                    null_mut(),
                )
            };

            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_HANDLE_EOF {
                    break;
                }
                return Err(io_error(code, "读取 USN Journal 失败"));
            }

            if returned as usize <= size_of::<i64>() {
                break;
            }

            let next_usn = unsafe { std::ptr::read_unaligned(buffer.as_ptr() as *const i64) };
            parse_records(&buffer[size_of::<i64>()..returned as usize], &mut records)?;
            input.StartUsn = next_usn;

            if next_usn >= target_usn {
                break;
            }
        }

        Ok(records)
    }

    fn query_usn_journal(handle: HANDLE) -> Result<USN_JOURNAL_DATA> {
        let mut journal = USN_JOURNAL_DATA::default();
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_USN_JOURNAL,
                null(),
                0,
                &mut journal as *mut _ as *mut _,
                size_of::<USN_JOURNAL_DATA>() as u32,
                &mut returned,
                null_mut(),
            )
        };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(io_error(code, "查询 USN Journal 失败"));
        }

        Ok(journal)
    }

    fn parse_records(buffer: &[u8], records: &mut Vec<NtfsRecord>) -> Result<()> {
        let mut offset = 0usize;
        while offset + size_of::<USN_RECORD>() <= buffer.len() {
            let ptr = unsafe { buffer.as_ptr().add(offset) };
            let record = unsafe { std::ptr::read_unaligned(ptr as *const USN_RECORD) };
            let record_len = record.RecordLength as usize;

            if record_len == 0 || offset + record_len > buffer.len() {
                break;
            }

            if record.MajorVersion == 2 {
                let name_start = offset + record.FileNameOffset as usize;
                let name_len = record.FileNameLength as usize / 2;
                if name_start + name_len * 2 <= buffer.len() {
                    let name_ptr = unsafe { buffer.as_ptr().add(name_start) as *const u16 };
                    let name = String::from_utf16_lossy(unsafe {
                        slice::from_raw_parts(name_ptr, name_len)
                    });
                    records.push(NtfsRecord {
                        file_ref: record.FileReferenceNumber,
                        parent_ref: record.ParentFileReferenceNumber,
                        name,
                        attributes: record.FileAttributes,
                        usn: record.Usn,
                        timestamp: record.TimeStamp,
                        reason: record.Reason,
                    });
                }
            }

            offset += record_len;
        }

        Ok(())
    }

    struct VolumeHandle(HANDLE);

    impl VolumeHandle {
        fn open(volume: &str) -> Result<Self> {
            let path = format!(r"\\.\{}", volume.trim_end_matches('\\'));
            let wide = wide_null(&path);
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
                let code = unsafe { GetLastError() };
                return Err(io_error(code, "打开卷失败"));
            }

            Ok(Self(handle))
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for VolumeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn normalize_volume(volume: &str) -> Result<String> {
        let trimmed = volume.trim().trim_end_matches('\\');
        if trimmed.len() != 2 || !trimmed.ends_with(':') {
            bail!("卷格式无效：{volume}");
        }
        let mut chars = trimmed.chars();
        let letter = chars.next().unwrap().to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() {
            bail!("卷格式无效：{volume}");
        }
        Ok(format!("{letter}:"))
    }

    fn drive_type_label(drive_type: u32) -> &'static str {
        match drive_type {
            DRIVE_FIXED => "本地磁盘",
            DRIVE_REMOVABLE => "可移动磁盘",
            4 => "网络磁盘",
            5 => "光驱",
            6 => "RAM 磁盘",
            _ => "未知",
        }
    }

    fn utf16_buffer_to_string(buffer: &[u16]) -> String {
        let end = buffer
            .iter()
            .position(|ch| *ch == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    fn last_error(context: &str) -> anyhow::Error {
        let code = unsafe { GetLastError() };
        io_error(code, context)
    }

    fn io_error(code: u32, context: &str) -> anyhow::Error {
        let detail = match code {
            ERROR_ACCESS_DENIED => "权限不足，请以管理员身份运行后重试。".to_string(),
            ERROR_JOURNAL_NOT_ACTIVE => "该卷未启用 USN Journal。".to_string(),
            ERROR_JOURNAL_ENTRY_DELETED => {
                "USN Journal 中需要的记录已被清理，请重建索引。".to_string()
            }
            _ => std::io::Error::from_raw_os_error(code as i32).to_string(),
        };
        anyhow::anyhow!("{context}：{detail} (错误码 {code})")
    }

    #[allow(dead_code)]
    fn is_directory(attributes: u32) -> bool {
        attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    use super::{FilterRules, NtfsIndexBuild, UsnChanges, VolumeIndexState, VolumeInfo};

    pub fn list_volumes() -> Result<Vec<VolumeInfo>> {
        Ok(Vec::new())
    }

    pub fn build_volume_index(_volume: String, _filter: &FilterRules) -> Result<NtfsIndexBuild> {
        bail!("MFT/USN 索引仅支持 Windows。")
    }

    pub fn read_usn_changes(_state: VolumeIndexState) -> Result<UsnChanges> {
        bail!("MFT/USN 索引仅支持 Windows。")
    }
}

pub use imp::{build_volume_index, list_volumes, read_usn_changes};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
