use std::{
    collections::HashMap,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use super::model::FileEntry;

#[derive(Debug, Clone)]
pub struct EntryMetadata {
    pub file_ref: u64,
    pub size: Option<u64>,
    pub modified: Option<u64>,
}

/// Reads standard filesystem metadata after the MFT/USN pass has reconstructed
/// absolute paths. USN records do not contain a file's logical size.
pub fn collect_file_metadata(entries: &[FileEntry]) -> Vec<EntryMetadata> {
    entries
        .iter()
        .map(|entry| {
            let metadata = fs::metadata(&entry.path).ok();
            let size = metadata
                .as_ref()
                .filter(|_| !entry.is_dir)
                .map(|metadata| metadata.len());
            let modified = metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_to_unix_ms);

            EntryMetadata {
                file_ref: entry.file_ref,
                size,
                modified,
            }
        })
        .collect()
}

pub fn hydrate_file_metadata(entries: &mut [FileEntry]) {
    let metadata = collect_file_metadata(entries);
    apply_file_metadata(entries, metadata);
}

/// 只给**搜索结果集**补大小/修改时间（≤ 返回上限，最多 500 条）。
///
/// 全量构建时绝不能逐条 stat：整卷百万文件 = 百万次同步 I/O，实测把磁盘
/// 打到 100%、单核 30%+、常驻内存 1.4GB。索引只存 USN/MFT 里有的东西
/// （名字、引用、父子关系、属性），大小这类"展示用"字段按需取。
pub fn hydrate_result_metadata(entries: &mut [FileEntry]) {
    for entry in entries.iter_mut() {
        if entry.is_dir || entry.size.is_some() {
            continue;
        }
        let Ok(metadata) = fs::metadata(&entry.path) else {
            continue;
        };
        entry.size = Some(metadata.len());
        if let Some(modified) = metadata.modified().ok().and_then(system_time_to_unix_ms) {
            entry.modified = modified;
        }
    }
}

pub fn apply_file_metadata(entries: &mut [FileEntry], metadata: Vec<EntryMetadata>) {
    let metadata_by_ref: HashMap<u64, EntryMetadata> = metadata
        .into_iter()
        .map(|entry| (entry.file_ref, entry))
        .collect();

    for entry in entries {
        let Some(metadata) = metadata_by_ref.get(&entry.file_ref) else {
            continue;
        };

        entry.size = metadata.size;
        if let Some(modified) = metadata.modified {
            entry.modified = modified;
        }
    }
}

fn system_time_to_unix_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn entry(path: String) -> FileEntry {
        FileEntry {
            id: 1,
            volume: "C:".to_string(),
            file_ref: 1,
            parent_ref: 0,
            name: "sample.txt".to_string(),
            path,
            extension: Some("txt".to_string()),
            size: None,
            modified: 0,
            is_dir: false,
            last_usn: 0,
            attributes: 0,
        }
    }

    /// 结果集补全是有界的旁路：目录不 stat、路径不存在也不炸。
    #[test]
    fn result_hydration_skips_dirs_and_missing_paths() {
        let directory = tempdir().unwrap();
        let dir_path = directory.path().display().to_string();
        let mut entries = vec![
            FileEntry {
                is_dir: true,
                name: "sub".to_string(),
                path: dir_path.clone(),
                ..entry(dir_path)
            },
            entry(r"Z:\definitely\missing\file.txt".to_string()),
        ];

        hydrate_result_metadata(&mut entries);

        assert_eq!(entries[0].size, None, "目录不该被 stat");
        assert_eq!(entries[1].size, None, "路径不存在时保持未知");
    }

    #[test]
    fn hydrates_size_and_modified_time_from_the_filesystem() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        fs::write(&path, b"FilePulse").unwrap();
        let mut entries = vec![entry(path.display().to_string())];

        hydrate_file_metadata(&mut entries);

        assert_eq!(entries[0].size, Some(9));
        assert!(entries[0].modified > 0);
    }
}
