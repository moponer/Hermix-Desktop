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
