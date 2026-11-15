//! 结果集的按需元数据。
//!
//! **索引里不存** size / 绝对路径这类展示字段——它们只对最终返回的那几百条
//! 有意义。构建期逐条 `fs::metadata` 是上一版的致命错误：全盘百万文件就是
//! 百万次同步 I/O，磁盘直接 100%、机器卡死。

use std::{fs, time::{SystemTime, UNIX_EPOCH}};

use super::model::FileEntry;

/// 只给**搜索结果集**补大小/修改时间（≤ 返回上限，最多 500 条）。
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

    #[test]
    fn hydrates_size_and_modified_time_from_the_filesystem() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        fs::write(&path, b"FilePulse").unwrap();
        let mut entries = vec![entry(path.display().to_string())];

        hydrate_result_metadata(&mut entries);

        assert_eq!(entries[0].size, Some(9));
        assert!(entries[0].modified > 0);
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
}
