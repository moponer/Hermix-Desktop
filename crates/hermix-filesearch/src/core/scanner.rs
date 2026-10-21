#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Result};
use walkdir::WalkDir;

use super::model::{BuildIndexSummary, FileEntry};

pub fn scan_directory(root: PathBuf) -> Result<(Vec<FileEntry>, BuildIndexSummary)> {
    if !root.exists() {
        bail!("路径不存在：{}", root.display());
    }

    if !root.is_dir() {
        bail!("路径不是文件夹：{}", root.display());
    }

    let start = Instant::now();
    let root_display = path_to_string(&root);
    let volume = fallback_volume(&root);
    let mut entries = Vec::new();
    let mut files = 0usize;
    let mut directories = 0usize;

    for item in WalkDir::new(&root).follow_links(false).into_iter() {
        let item = match item {
            Ok(item) => item,
            Err(_) => continue,
        };

        let metadata = match item.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        let path = item.path();
        let is_dir = metadata.is_dir();

        if is_dir {
            directories += 1;
        } else {
            files += 1;
        }

        let id = entries.len() as u64 + 1;
        entries.push(FileEntry {
            id,
            volume: volume.clone(),
            file_ref: id,
            parent_ref: 0,
            name: file_name(path),
            path: path_to_string(path),
            extension: extension(path, is_dir),
            size: if is_dir { None } else { Some(metadata.len()) },
            modified: modified_ms(metadata.modified().ok()),
            is_dir,
            last_usn: 0,
            attributes: if is_dir { 16 } else { 0 },
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.file_ref.cmp(&b.file_ref))
    });

    let summary = BuildIndexSummary {
        volume: root_display,
        total_entries: entries.len(),
        files,
        directories,
        elapsed_ms: start.elapsed().as_millis() as u64,
        journal_id: 0,
        next_usn: 0,
        snapshot_saved: false,
        // 目录遍历回退路径没有分阶段耗时（MFT 路径才有）。
        scan_ms: start.elapsed().as_millis() as u64,
        size_ms: 0,
        index_ms: 0,
        save_ms: 0,
    };

    Ok((entries, summary))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_to_string(path))
}

fn extension(path: &Path, is_dir: bool) -> Option<String> {
    if is_dir {
        return None;
    }
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn modified_ms(modified: Option<SystemTime>) -> u64 {
    modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn fallback_volume(root: &Path) -> String {
    let value = path_to_string(root);
    if value.len() >= 2 && value.as_bytes()[1] == b':' {
        value[..2].to_string()
    } else {
        "walkdir".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn scans_files_and_directories() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("src")).expect("mkdir");
        fs::write(dir.path().join("src").join("main.rs"), "fn main() {}").expect("write");
        fs::write(dir.path().join("README.md"), "hello").expect("write");

        let (entries, summary) = scan_directory(dir.path().to_path_buf()).expect("scan");

        assert_eq!(summary.total_entries, entries.len());
        assert_eq!(summary.files, 2);
        assert!(summary.directories >= 2);
        assert!(entries.iter().any(|entry| entry.name == "main.rs"));
        assert!(entries
            .iter()
            .any(|entry| entry.extension.as_deref() == Some("rs")));
    }

    #[test]
    fn rejects_non_directory_roots() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("note.txt");
        fs::write(&file, "hello").expect("write");

        let error = scan_directory(file).expect_err("should reject file roots");

        assert!(error.to_string().contains("路径不是文件夹"));
    }
}
