use std::collections::{HashMap, HashSet};

use windows_sys::Win32::System::Ioctl::{USN_REASON_FILE_DELETE, USN_REASON_RENAME_OLD_NAME};

use super::{
    metadata::{apply_file_metadata, EntryMetadata},
    model::{
        ChangeStats, FileEntry, FilterRules, IndexSnapshot, IndexStatus, NtfsRecord,
        VolumeIndexState, SNAPSHOT_VERSION,
    },
    search::{compare_entries, search_entries, searchable_text},
};

#[derive(Debug, Default)]
pub struct SearchIndex {
    volume_state: Option<VolumeIndexState>,
    entries: Vec<FileEntry>,
    searchable: Vec<String>,
    filter: FilterRules,
    last_scan_ms: Option<u64>,
    snapshot_loaded: bool,
    snapshot_path: Option<String>,
    needs_rebuild: bool,
    status_message: Option<String>,
}

impl SearchIndex {
    pub fn set_filter(&mut self, filter: FilterRules) {
        self.filter = filter;
    }
    pub fn replace_volume(
        &mut self,
        volume_state: VolumeIndexState,
        mut entries: Vec<FileEntry>,
        last_scan_ms: u64,
        snapshot_path: Option<String>,
    ) {
        entries.sort_by(compare_entries);
        self.volume_state = Some(volume_state);
        self.entries = entries;
        self.last_scan_ms = Some(last_scan_ms);
        self.snapshot_loaded = false;
        self.snapshot_path = snapshot_path;
        self.needs_rebuild = false;
        self.status_message = None;
        self.rebuild_maps();
    }

    pub fn replace_snapshot(&mut self, snapshot: IndexSnapshot, snapshot_path: String) {
        let mut entries = snapshot.entries;
        entries.sort_by(compare_entries);
        self.volume_state = Some(snapshot.volume_state);
        self.entries = entries;
        self.last_scan_ms = None;
        self.snapshot_loaded = true;
        self.snapshot_path = Some(snapshot_path);
        self.needs_rebuild = false;
        self.status_message = Some("已加载上次索引快照，可直接搜索。".to_string());
        self.rebuild_maps();
    }

    pub fn search(&self, query: String, limit: Option<usize>) -> super::model::SearchResponse {
        search_entries(&self.entries, &self.searchable, query, limit)
    }

    pub fn status(&self) -> IndexStatus {
        IndexStatus {
            root: self
                .volume_state
                .as_ref()
                .map(|state| format!("{}\\", state.volume)),
            volume: self.volume_state.as_ref().map(|state| state.volume.clone()),
            total_entries: self.entries.len(),
            is_indexed: self.volume_state.is_some(),
            last_scan_ms: self.last_scan_ms,
            snapshot_loaded: self.snapshot_loaded,
            snapshot_path: self.snapshot_path.clone(),
            journal_id: self.volume_state.as_ref().map(|state| state.journal_id),
            next_usn: self.volume_state.as_ref().map(|state| state.next_usn),
            index_source: self.volume_state.as_ref().map(|state| state.source.clone()),
            needs_rebuild: self.needs_rebuild,
            status_message: self.status_message.clone(),
            monitor_state: super::model::MonitorState::Idle,
            last_sync_at: None,
            last_sync_error: None,
        }
    }

    pub fn current_snapshot(&self) -> Option<IndexSnapshot> {
        self.volume_state.as_ref().map(|state| IndexSnapshot {
            version: SNAPSHOT_VERSION,
            volume_state: state.clone(),
            entries: self.entries.clone(),
        })
    }

    pub fn volume_state(&self) -> Option<VolumeIndexState> {
        self.volume_state.clone()
    }

    pub fn needs_rebuild(&self) -> bool {
        self.needs_rebuild
    }

    pub fn advance_sync_cursor(&mut self, next_usn: i64, synced_at: u64) -> bool {
        let Some(state) = self.volume_state.as_mut() else {
            return false;
        };
        if state.next_usn == next_usn {
            return false;
        }

        state.next_usn = next_usn;
        state.last_synced_at = synced_at;
        self.snapshot_loaded = false;
        self.status_message = Some("已同步 USN Journal 游标。".to_string());
        true
    }

    pub fn mark_needs_rebuild(&mut self, message: String) {
        self.needs_rebuild = true;
        self.status_message = Some(message);
    }

    pub fn set_snapshot_path(&mut self, snapshot_path: Option<String>) {
        self.snapshot_path = snapshot_path;
    }

    pub fn set_status_message(&mut self, message: Option<String>) {
        self.status_message = message;
    }

    pub fn entries_for_file_refs(&self, file_refs: &HashSet<u64>) -> Vec<FileEntry> {
        self.entries
            .iter()
            .filter(|entry| file_refs.contains(&entry.file_ref))
            .cloned()
            .collect()
    }

    pub fn apply_metadata(&mut self, metadata: Vec<EntryMetadata>) {
        apply_file_metadata(&mut self.entries, metadata);
    }

    pub fn apply_changes(
        &mut self,
        records: Vec<NtfsRecord>,
        next_usn: i64,
        synced_at: u64,
    ) -> ChangeStats {
        let Some(state) = self.volume_state.as_mut() else {
            return ChangeStats::default();
        };

        let volume = state.volume.clone();
        let mut entries_by_ref: HashMap<u64, FileEntry> = self
            .entries
            .drain(..)
            .map(|entry| (entry.file_ref, entry))
            .collect();
        let mut stats = ChangeStats::default();

        for record in records {
            if record.reason & USN_REASON_RENAME_OLD_NAME != 0 {
                continue;
            }

            stats.applied += 1;

            if record.reason & USN_REASON_FILE_DELETE != 0 {
                let children = Self::children_map_from_entries(entries_by_ref.values());
                let refs_to_remove = Self::collect_subtree_refs(record.file_ref, &children);
                for file_ref in refs_to_remove {
                    if entries_by_ref.remove(&file_ref).is_some() {
                        stats.deleted += 1;
                    }
                }
                continue;
            }

            let is_dir = is_directory(record.attributes);
            let extension = extension(&record.name, is_dir);
            let modified = filetime_to_unix_ms(record.timestamp);

            match entries_by_ref.get_mut(&record.file_ref) {
                Some(entry) => {
                    entry.parent_ref = record.parent_ref;
                    entry.name = record.name;
                    entry.extension = extension;
                    entry.modified = modified;
                    entry.is_dir = is_dir;
                    entry.last_usn = record.usn;
                    entry.attributes = record.attributes;
                    stats.updated += 1;
                }
                None => {
                    entries_by_ref.insert(
                        record.file_ref,
                        FileEntry {
                            id: record.file_ref,
                            volume: volume.clone(),
                            file_ref: record.file_ref,
                            parent_ref: record.parent_ref,
                            name: record.name,
                            path: String::new(),
                            extension,
                            size: None,
                            modified,
                            is_dir,
                            last_usn: record.usn,
                            attributes: record.attributes,
                        },
                    );
                    stats.updated += 1;
                }
            }
        }

        // 增量期过滤：与构建期同一谓词（FilterRules::accepts）。
        // 目录排除走子树剪枝，否则被排除目录的子文件会凭 parent_ref 继续存活。
        let excluded_roots: Vec<u64> = entries_by_ref
            .values()
            .filter(|entry| !self.filter.accepts(&entry.name, entry.is_dir, entry.attributes))
            .map(|entry| entry.file_ref)
            .collect();
        if !excluded_roots.is_empty() {
            let children = Self::children_map_from_entries(entries_by_ref.values());
            for root in excluded_roots {
                for file_ref in Self::collect_subtree_refs(root, &children) {
                    if entries_by_ref.remove(&file_ref).is_some() {
                        stats.deleted += 1;
                    }
                }
            }
        }

        rebuild_paths(&volume, &mut entries_by_ref);
        let mut entries: Vec<FileEntry> = entries_by_ref.into_values().collect();
        entries.sort_by(compare_entries);
        self.entries = entries;
        state.next_usn = next_usn;
        state.last_synced_at = synced_at;
        self.snapshot_loaded = false;
        self.needs_rebuild = false;
        self.status_message = Some("已同步 USN Journal 变化。".to_string());
        self.rebuild_maps();

        stats
    }

    fn rebuild_maps(&mut self) {
        self.searchable.clear();
        self.searchable
            .extend(self.entries.iter().map(searchable_text));
    }

    fn children_map_from_entries<'a>(
        entries: impl Iterator<Item = &'a FileEntry>,
    ) -> HashMap<u64, Vec<u64>> {
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for entry in entries {
            children
                .entry(entry.parent_ref)
                .or_default()
                .push(entry.file_ref);
        }
        children
    }

    fn collect_subtree_refs(root: u64, children: &HashMap<u64, Vec<u64>>) -> Vec<u64> {
        let mut result = Vec::new();
        let mut stack = vec![root];
        let mut seen = HashSet::new();

        while let Some(file_ref) = stack.pop() {
            if !seen.insert(file_ref) {
                continue;
            }
            result.push(file_ref);
            if let Some(child_refs) = children.get(&file_ref) {
                stack.extend(child_refs.iter().copied());
            }
        }

        result
    }
}

pub fn build_entries_from_records(
    volume: &str,
    records: Vec<NtfsRecord>,
    filter: &FilterRules,
) -> Vec<FileEntry> {
    let mut entries_by_ref: HashMap<u64, FileEntry> = records
        .into_iter()
        .filter(|record| !record.name.is_empty())
        .map(|record| {
            let is_dir = is_directory(record.attributes);
            let entry = FileEntry {
                id: record.file_ref,
                volume: volume.to_string(),
                file_ref: record.file_ref,
                parent_ref: record.parent_ref,
                name: record.name.clone(),
                path: String::new(),
                extension: extension(&record.name, is_dir),
                size: None,
                modified: filetime_to_unix_ms(record.timestamp),
                is_dir,
                last_usn: record.usn,
                attributes: record.attributes,
            };
            (entry.file_ref, entry)
        })
        .collect();

    // 构建期过滤：排除目录按子树剪枝；排除文件/扩展名/隐藏系统位按单条剔除。
    let excluded_roots: Vec<u64> = entries_by_ref
        .values()
        .filter(|entry| !filter.accepts(&entry.name, entry.is_dir, entry.attributes))
        .map(|entry| entry.file_ref)
        .collect();
    if !excluded_roots.is_empty() {
        let children: HashMap<u64, Vec<u64>> = entries_by_ref.values().fold(
            HashMap::new(),
            |mut acc, entry| {
                acc.entry(entry.parent_ref).or_default().push(entry.file_ref);
                acc
            },
        );
        for root in excluded_roots {
            for file_ref in SearchIndex::collect_subtree_refs(root, &children) {
                entries_by_ref.remove(&file_ref);
            }
        }
    }

    rebuild_paths(volume, &mut entries_by_ref);

    let mut entries: Vec<FileEntry> = entries_by_ref.into_values().collect();
    entries.sort_by(compare_entries);
    entries
}

fn rebuild_paths(volume: &str, entries_by_ref: &mut HashMap<u64, FileEntry>) {
    let raw: HashMap<u64, (u64, String)> = entries_by_ref
        .iter()
        .map(|(file_ref, entry)| (*file_ref, (entry.parent_ref, entry.name.clone())))
        .collect();
    let mut memo: HashMap<u64, String> = HashMap::new();

    for file_ref in raw.keys().copied().collect::<Vec<_>>() {
        let path = resolve_path(volume, file_ref, &raw, &mut memo, &mut HashSet::new());
        if let Some(entry) = entries_by_ref.get_mut(&file_ref) {
            entry.path = path;
        }
    }
}

fn resolve_path(
    volume: &str,
    file_ref: u64,
    raw: &HashMap<u64, (u64, String)>,
    memo: &mut HashMap<u64, String>,
    visiting: &mut HashSet<u64>,
) -> String {
    if let Some(path) = memo.get(&file_ref) {
        return path.clone();
    }

    if !visiting.insert(file_ref) {
        return volume_root(volume);
    }

    let Some((parent_ref, name)) = raw.get(&file_ref) else {
        return volume_root(volume);
    };

    let path = if *parent_ref == file_ref || name == "." || name.is_empty() {
        volume_root(volume)
    } else if !raw.contains_key(parent_ref) {
        join_path(&volume_root(volume), name)
    } else {
        let parent = resolve_path(volume, *parent_ref, raw, memo, visiting);
        join_path(&parent, name)
    };

    visiting.remove(&file_ref);
    memo.insert(file_ref, path.clone());
    path
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.ends_with('\\') {
        format!("{parent}{name}")
    } else {
        format!("{parent}\\{name}")
    }
}

fn volume_root(volume: &str) -> String {
    if volume.ends_with('\\') {
        volume.to_string()
    } else {
        format!("{}\\", volume.trim_end_matches('\\'))
    }
}

fn is_directory(attributes: u32) -> bool {
    attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY != 0
}

fn extension(name: &str, is_dir: bool) -> Option<String> {
    if is_dir {
        return None;
    }
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty() && ext != name)
}

fn filetime_to_unix_ms(filetime: i64) -> u64 {
    const WINDOWS_TO_UNIX_EPOCH_MS: i64 = 11_644_473_600_000;
    if filetime <= 0 {
        return 0;
    }
    let ms = filetime / 10_000 - WINDOWS_TO_UNIX_EPOCH_MS;
    ms.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_default(volume: &str, records: Vec<NtfsRecord>) -> Vec<FileEntry> {
        build_entries_from_records(volume, records, &FilterRules::default())
    }

    fn record(file_ref: u64, parent_ref: u64, name: &str, attributes: u32) -> NtfsRecord {
        NtfsRecord {
            file_ref,
            parent_ref,
            name: name.to_string(),
            attributes,
            usn: 1,
            timestamp: 0,
            reason: 0,
        }
    }

    #[test]
    fn rebuilds_paths_from_parent_refs() {
        let entries = build_default(
            "F:",
            vec![
                record(5, 5, ".", 16),
                record(10, 5, "src", 16),
                record(11, 10, "main.rs", 0),
            ],
        );

        assert!(entries.iter().any(|entry| entry.path == r"F:\src\main.rs"));
    }

    #[test]
    fn directory_rename_refreshes_child_paths() {
        let state = VolumeIndexState {
            volume: "F:".to_string(),
            journal_id: 1,
            first_usn: 0,
            next_usn: 1,
            indexed_at: 0,
            last_synced_at: 0,
            source: "MFT".to_string(),
        };
        let mut index = SearchIndex::default();
        let entries = build_default(
            "F:",
            vec![
                record(5, 5, ".", 16),
                record(10, 5, "old", 16),
                record(11, 10, "file.txt", 0),
            ],
        );
        index.replace_volume(state, entries, 0, None);

        index.apply_changes(vec![record(10, 5, "new", 16)], 2, 0);

        let response = index.search("file".to_string(), None);
        assert_eq!(response.results[0].path, r"F:\new\file.txt");
    }

    #[test]
    fn directory_delete_removes_children() {
        let state = VolumeIndexState {
            volume: "F:".to_string(),
            journal_id: 1,
            first_usn: 0,
            next_usn: 1,
            indexed_at: 0,
            last_synced_at: 0,
            source: "MFT".to_string(),
        };
        let mut index = SearchIndex::default();
        let entries = build_default(
            "F:",
            vec![
                record(5, 5, ".", 16),
                record(10, 5, "folder", 16),
                record(11, 10, "file.txt", 0),
            ],
        );
        index.replace_volume(state, entries, 0, None);

        let mut delete = record(10, 5, "folder", 16);
        delete.reason = USN_REASON_FILE_DELETE;
        index.apply_changes(vec![delete], 2, 0);

        let response = index.search("file".to_string(), None);
        assert_eq!(response.total_matches, 0);
    }

    #[test]
    fn advances_sync_cursor_without_rebuilding_entries() {
        let state = VolumeIndexState {
            volume: "F:".to_string(),
            journal_id: 1,
            first_usn: 0,
            next_usn: 1,
            indexed_at: 0,
            last_synced_at: 0,
            source: "MFT".to_string(),
        };
        let mut index = SearchIndex::default();
        let entries = build_default(
            "F:",
            vec![record(5, 5, ".", 16), record(10, 5, "file.txt", 0)],
        );
        index.replace_volume(state, entries, 0, None);

        assert!(index.advance_sync_cursor(2, 100));
        assert_eq!(index.volume_state().unwrap().next_usn, 2);
        assert_eq!(index.search("file".to_string(), None).total_matches, 1);
        assert!(!index.advance_sync_cursor(2, 200));
    }

    #[test]
    fn build_filters_excluded_dir_with_subtree() {
        let mut rules = FilterRules::default();
        rules.excluded_dirs = vec!["node_modules".to_string()];
        let entries = build_entries_from_records(
            "F:",
            vec![
                record(5, 5, ".", 16),
                record(10, 5, "src", 16),
                record(11, 10, "main.rs", 0),
                record(20, 5, "node_modules", 16),
                record(21, 20, "left-pad", 16),
                record(22, 21, "index.js", 0),
            ],
            &rules,
        );

        assert!(entries.iter().any(|entry| entry.path == r"F:\src\main.rs"));
        assert!(entries.iter().all(|entry| !entry.path.contains("node_modules")));
    }

    #[test]
    fn apply_changes_rejects_entries_under_excluded_dir() {
        let mut rules = FilterRules::default();
        rules.excluded_dirs = vec!["node_modules".to_string()];
        let state = VolumeIndexState {
            volume: "F:".to_string(),
            journal_id: 1,
            first_usn: 0,
            next_usn: 1,
            indexed_at: 0,
            last_synced_at: 0,
            source: "MFT".to_string(),
        };
        let mut index = SearchIndex::default();
        index.set_filter(rules);
        let entries = build_default(
            "F:",
            vec![record(5, 5, ".", 16), record(10, 5, "app.txt", 0)],
        );
        index.replace_volume(state, entries, 0, None);

        // USN 增量新增 node_modules 目录及其子文件 → 必须整棵拒绝，不许回流。
        index.apply_changes(
            vec![record(20, 5, "node_modules", 16), record(21, 20, "pkg.js", 0)],
            2,
            0,
        );

        let response = index.search("pkg".to_string(), None);
        assert_eq!(response.total_matches, 0);
        assert_eq!(index.search("app".to_string(), None).total_matches, 1);
    }

    #[test]
    fn filter_skips_hidden_system_files_but_keeps_dirs() {
        let rules = FilterRules::default();
        // pagefile.sys：隐藏+系统 → 拒绝
        assert!(!rules.accepts("pagefile.sys", false, 0x22));
        // 普通隐藏文件（仅 hidden）→ 接受
        assert!(rules.accepts(".env", false, 0x2));
        // 目录不受 hidden+system 位影响（目录过滤只走 excluded_dirs 名称匹配）
        assert!(rules.accepts("some_sys_dir", true, 0x16));
        // 目录名命中 excluded_dirs → 拒绝（与属性位无关）
        assert!(!rules.accepts("System Volume Information", true, 0x16));
    }
}
