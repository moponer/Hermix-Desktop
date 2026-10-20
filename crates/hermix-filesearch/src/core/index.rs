//! 内存索引：Everything 式布局。
//!
//! 关键约束（对齐开源 Everything 类实现 Lertaro / SwiftList 的做法）：
//! - 索引里**只存** `(file_ref, parent_ref, name)`，不存绝对路径；
//!   路径在查询时按 parent 链回溯，只拼最终返回的那几百条。
//! - USN 增量**原地改**这几条，不重建全表、不重排全表、不重建路径。
//! - 搜索只扫名字，先收集命中下标再排序，不 clone 条目。
//!
//! 之前的三处结构性错误正是卡死机器的根源：
//! 1) 每条存 path（每个目录前缀重复存 N 遍）；
//! 2) 另存一份全量小写副本 `searchable`；
//! 3) 每秒一次的 USN 同步里 `rebuild_paths` + 全表 `sort` + 重建小写表。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use windows_sys::Win32::System::Ioctl::{USN_REASON_FILE_DELETE, USN_REASON_RENAME_OLD_NAME};

use super::{
    model::{
        ChangeStats, FileEntry, FilterRules, IndexEntry, IndexSnapshot, IndexStatus, NtfsRecord,
        SearchResponse, VolumeIndexState, SNAPSHOT_VERSION,
    },
    search::compare_hits,
};

#[derive(Debug, Default)]
pub struct SearchIndex {
    volume_state: Option<VolumeIndexState>,
    entries: Vec<IndexEntry>,
    /// `file_ref → entries` 下标：增量更新与路径回溯都靠它，避免全表扫描。
    by_ref: HashMap<u64, u32>,
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
        entries: Vec<IndexEntry>,
        last_scan_ms: u64,
        snapshot_path: Option<String>,
    ) {
        self.volume_state = Some(volume_state);
        self.entries = entries;
        self.last_scan_ms = Some(last_scan_ms);
        self.snapshot_loaded = false;
        self.snapshot_path = snapshot_path;
        self.needs_rebuild = false;
        self.status_message = None;
        self.reindex();
    }

    pub fn replace_snapshot(&mut self, snapshot: IndexSnapshot, snapshot_path: String) {
        self.volume_state = Some(snapshot.volume_state);
        self.entries = snapshot.entries;
        self.last_scan_ms = None;
        self.snapshot_loaded = true;
        self.snapshot_path = Some(snapshot_path);
        self.needs_rebuild = false;
        self.status_message = Some("已加载上次索引快照，可直接搜索。".to_string());
        self.reindex();
    }

    fn reindex(&mut self) {
        self.by_ref.clear();
        self.by_ref.reserve(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            self.by_ref.insert(entry.file_ref, index as u32);
        }
    }

    /// 搜名字：先收集命中下标，排序后再为前 `limit` 条拼路径。
    pub fn search(&self, query: String, limit: Option<usize>) -> SearchResponse {
        let start = Instant::now();
        let normalized = query.trim().to_lowercase();
        let limit = limit.unwrap_or(500).clamp(1, 5_000);

        // 按排名分桶收集：精确名 / 前缀 / 子串。只保留"够用"的子串桶
        // （上限 = limit），因此输入单字母也不会为几十万条命中分配 + 全量排序。
        let mut exact: Vec<u32> = Vec::new();
        let mut prefix: Vec<u32> = Vec::new();
        let mut other: Vec<u32> = Vec::new();
        let mut total_matches = 0usize;

        if normalized.is_empty() {
            total_matches = self.entries.len();
            other.extend((0..self.entries.len().min(limit)).map(|index| index as u32));
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                if !entry.name_lower.contains(&normalized) {
                    continue;
                }
                total_matches += 1;
                if entry.name_lower == normalized {
                    exact.push(index as u32);
                } else if entry.name_lower.starts_with(&normalized) {
                    prefix.push(index as u32);
                } else if other.len() < limit {
                    other.push(index as u32);
                }
            }
        }

        // 只对"会被返回"的这几桶排序，不再对全部命中排序。
        if !normalized.is_empty() {
            let order = |a: &u32, b: &u32| {
                compare_hits(
                    &self.entries[*a as usize],
                    &self.entries[*b as usize],
                    &normalized,
                )
            };
            exact.sort_by(order);
            prefix.sort_by(order);
            other.sort_by(order);
        }

        let mut picked: Vec<u32> = Vec::with_capacity(limit.min(total_matches));
        for bucket in [&exact, &prefix, &other] {
            for index in bucket {
                if picked.len() >= limit {
                    break;
                }
                picked.push(*index);
            }
        }

        let mut memo: HashMap<u32, String> = HashMap::new();
        let results: Vec<FileEntry> = picked
            .iter()
            .map(|index| self.to_file_entry(*index, &mut memo))
            .collect();

        SearchResponse {
            query,
            total_matches,
            returned: results.len(),
            results,
            elapsed_ms: start.elapsed().as_millis() as u64,
        }
    }

    fn to_file_entry(&self, index: u32, memo: &mut HashMap<u32, String>) -> FileEntry {
        let entry = &self.entries[index as usize];
        FileEntry {
            id: entry.file_ref,
            volume: self
                .volume_state
                .as_ref()
                .map(|state| state.volume.clone())
                .unwrap_or_default(),
            file_ref: entry.file_ref,
            parent_ref: entry.parent_ref,
            name: entry.name.clone(),
            path: self.path_of(index, memo),
            extension: extension(&entry.name, entry.is_dir),
            size: None,
            modified: entry.modified,
            is_dir: entry.is_dir,
            last_usn: entry.last_usn,
            attributes: entry.attributes,
        }
    }

    /// 沿 `parent_ref` 向上收集名字再拼路径。只在结果集上调用（≤500 条），
    /// 单条深度是目录层数（几十），代价可忽略。
    fn path_of(&self, index: u32, memo: &mut HashMap<u32, String>) -> String {
        if let Some(path) = memo.get(&index) {
            return path.clone();
        }
        let volume = self
            .volume_state
            .as_ref()
            .map(|state| state.volume.as_str())
            .unwrap_or("C:");

        let mut names: Vec<&str> = Vec::new();
        let mut cursor = index;
        // 防御：损坏/环状 parent 链不能让回溯死循环。
        for _ in 0..256 {
            let entry = &self.entries[cursor as usize];
            if entry.parent_ref == entry.file_ref || entry.name.is_empty() || entry.name == "." {
                break;
            }
            names.push(entry.name.as_str());
            match self.by_ref.get(&entry.parent_ref).copied() {
                Some(parent) => cursor = parent,
                None => break,
            }
        }

        let mut path = volume_root(volume);
        for name in names.iter().rev() {
            path = join_path(&path, name);
        }
        memo.insert(index, path.clone());
        path
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

    /// 借引用取出快照原料（不 clone 条目）。落盘路径用这个：`current_snapshot`
    /// 会把整份索引复制一遍，周期性刷盘时等于制造内存尖峰。
    pub fn snapshot_parts(&self) -> Option<(&VolumeIndexState, &[IndexEntry])> {
        self.volume_state
            .as_ref()
            .map(|state| (state, self.entries.as_slice()))
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

    pub fn total_entries(&self) -> usize {
        self.entries.len()
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

    /// USN 增量：**原地改**命中条目，不重建路径 / 不重排全表 / 不重建小写副本。
    pub fn apply_changes(
        &mut self,
        records: Vec<NtfsRecord>,
        next_usn: i64,
        synced_at: u64,
    ) -> ChangeStats {
        if self.volume_state.is_none() {
            return ChangeStats::default();
        }
        let mut stats = ChangeStats::default();

        for record in records {
            // 重命名的"旧名字"记录没有新名字，等 NEW_NAME 那条处理。
            if record.reason & USN_REASON_RENAME_OLD_NAME != 0 {
                continue;
            }

            stats.applied += 1;

            if record.reason & USN_REASON_FILE_DELETE != 0 {
                let removed = if is_directory(record.attributes) {
                    let refs = self.subtree_refs(record.file_ref);
                    refs.into_iter()
                        .filter(|file_ref| self.remove_entry(*file_ref))
                        .count()
                } else if self.remove_entry(record.file_ref) {
                    1
                } else {
                    0
                };
                stats.deleted += removed;
                continue;
            }

            let is_dir = is_directory(record.attributes);
            let modified = filetime_to_unix_ms(record.timestamp);
            let name_lower = record.name.to_lowercase();

            match self.by_ref.get(&record.file_ref).copied() {
                Some(index) => {
                    let entry = &mut self.entries[index as usize];
                    entry.parent_ref = record.parent_ref;
                    entry.name = record.name;
                    entry.name_lower = name_lower;
                    entry.modified = modified;
                    entry.is_dir = is_dir;
                    entry.last_usn = record.usn;
                    entry.attributes = record.attributes;
                    stats.updated += 1;
                }
                None => {
                    // 增量期过滤：与全量构建同一谓词，避免被排除项经增量回流。
                    if !self
                        .filter
                        .accepts(&record.name, is_dir, record.attributes)
                    {
                        continue;
                    }
                    self.by_ref
                        .insert(record.file_ref, self.entries.len() as u32);
                    self.entries.push(IndexEntry {
                        file_ref: record.file_ref,
                        parent_ref: record.parent_ref,
                        name: record.name,
                        name_lower,
                        modified,
                        is_dir,
                        last_usn: record.usn,
                        attributes: record.attributes,
                    });
                    stats.updated += 1;
                }
            }
        }

        if let Some(state) = self.volume_state.as_mut() {
            state.next_usn = next_usn;
            state.last_synced_at = synced_at;
        }
        self.snapshot_loaded = false;
        self.needs_rebuild = false;
        self.status_message = Some("已同步 USN Journal 变化。".to_string());

        stats
    }

    /// 删除一条：`swap_remove` 是 O(1)，只需修正被换过来的那条下标。
    fn remove_entry(&mut self, file_ref: u64) -> bool {
        let Some(index) = self.by_ref.remove(&file_ref) else {
            return false;
        };
        let last = self.entries.len() - 1;
        if index as usize != last {
            self.entries.swap_remove(index as usize);
            let moved = &self.entries[index as usize];
            self.by_ref.insert(moved.file_ref, index);
        } else {
            self.entries.pop();
        }
        true
    }

    fn subtree_refs(&self, root: u64) -> Vec<u64> {
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for entry in &self.entries {
            children
                .entry(entry.parent_ref)
                .or_default()
                .push(entry.file_ref);
        }
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut seen = HashSet::new();
        while let Some(file_ref) = stack.pop() {
            if !seen.insert(file_ref) {
                continue;
            }
            out.push(file_ref);
            if let Some(child_refs) = children.get(&file_ref) {
                stack.extend(child_refs.iter().copied());
            }
        }
        out
    }
}

/// 全量构建：MFT 记录 → 紧凑索引条目。**不拼路径**（Everything 也不拼）。
pub fn build_entries_from_records(
    _volume: &str,
    records: Vec<NtfsRecord>,
    filter: &FilterRules,
) -> Vec<IndexEntry> {
    let mut kept: Vec<IndexEntry> = records
        .into_iter()
        .filter(|record| !record.name.is_empty())
        .map(|record| {
            let is_dir = is_directory(record.attributes);
            IndexEntry {
                file_ref: record.file_ref,
                parent_ref: record.parent_ref,
                name_lower: record.name.to_lowercase(),
                name: record.name,
                modified: filetime_to_unix_ms(record.timestamp),
                is_dir,
                last_usn: record.usn,
                attributes: record.attributes,
            }
        })
        .collect();

    // 构建期过滤：排除目录按子树剪枝；文件按谓词单条剔除。
    let excluded_roots: Vec<u64> = kept
        .iter()
        .filter(|entry| !filter.accepts(&entry.name, entry.is_dir, entry.attributes))
        .map(|entry| entry.file_ref)
        .collect();
    if !excluded_roots.is_empty() {
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for entry in &kept {
            children
                .entry(entry.parent_ref)
                .or_default()
                .push(entry.file_ref);
        }
        let mut drop_refs: HashSet<u64> = HashSet::new();
        for root in excluded_roots {
            let mut stack = vec![root];
            while let Some(file_ref) = stack.pop() {
                if !drop_refs.insert(file_ref) {
                    continue;
                }
                if let Some(child_refs) = children.get(&file_ref) {
                    stack.extend(child_refs.iter().copied());
                }
            }
        }
        kept.retain(|entry| !drop_refs.contains(&entry.file_ref));
    }

    kept
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
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|ext| !ext.is_empty() && ext.len() <= 16)
}

fn filetime_to_unix_ms(timestamp: i64) -> u64 {
    if timestamp <= 0 {
        return 0;
    }
    const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
    if timestamp <= FILETIME_UNIX_EPOCH {
        return timestamp.max(0) as u64;
    }
    ((timestamp - FILETIME_UNIX_EPOCH) / 10_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file_ref: u64, parent_ref: u64, name: &str, is_dir: bool) -> IndexEntry {
        IndexEntry {
            file_ref,
            parent_ref,
            name: name.to_string(),
            name_lower: name.to_lowercase(),
            modified: 0,
            is_dir,
            last_usn: 0,
            attributes: if is_dir { 16 } else { 0 },
        }
    }

    fn index_with(entries: Vec<IndexEntry>) -> SearchIndex {
        let mut index = SearchIndex::default();
        index.replace_volume(
            VolumeIndexState {
                volume: "C:".to_string(),
                journal_id: 1,
                first_usn: 0,
                next_usn: 0,
                indexed_at: 0,
                last_synced_at: 0,
                source: "test".to_string(),
            },
            entries,
            0,
            None,
        );
        index
    }

    #[test]
    fn resolves_paths_lazily_from_parent_chain() {
        // 5 = C:\ 根；6 = C:\repo；7 = C:\repo\src；8 = C:\repo\src\main.rs
        let index = index_with(vec![
            entry(5, 5, "", true),
            entry(6, 5, "repo", true),
            entry(7, 6, "src", true),
            entry(8, 7, "main.rs", false),
        ]);

        let response = index.search("main".to_string(), None);
        assert_eq!(response.total_matches, 1);
        assert_eq!(response.results[0].path, r"C:\repo\src\main.rs");
        assert_eq!(response.results[0].name, "main.rs");
    }

    #[test]
    fn sync_updates_in_place_without_rebuilding_other_paths() {
        let index = index_with(vec![
            entry(5, 5, "", true),
            entry(6, 5, "repo", true),
            entry(7, 6, "a.txt", false),
        ]);
        let mut index = index;

        // 目录改名：子条目完全不用动（路径是查询时拼的）。
        let stats = index.apply_changes(
            vec![super::NtfsRecord {
                file_ref: 6,
                parent_ref: 5,
                name: "repo2".to_string(),
                attributes: 16,
                usn: 10,
                timestamp: 0,
                reason: 0,
            }],
            10,
            1,
        );
        assert_eq!(stats.updated, 1);

        let response = index.search("a.txt".to_string(), None);
        assert_eq!(response.results[0].path, r"C:\repo2\a.txt");
    }

    /// 分桶：精确名 > 前缀 > 子串；`total_matches` 仍然统计全部命中，
    /// 只是不为子串桶分配/排序超过 limit 的条目。
    #[test]
    fn exact_and_prefix_win_and_total_counts_everything() {
        let index = index_with(vec![
            entry(5, 5, "", true),
            entry(6, 5, "app", true),
            entry(7, 5, "app.log", false),
            entry(8, 5, "myapp.txt", false),
        ]);

        let response = index.search("app".to_string(), Some(2));
        assert_eq!(response.total_matches, 3, "命中计数不截断");
        assert_eq!(response.returned, 2);
        assert_eq!(response.results[0].name, "app");
        assert_eq!(response.results[1].name, "app.log");
    }

    #[test]
    fn delete_removes_subtree() {
        let mut index = index_with(vec![
            entry(5, 5, "", true),
            entry(6, 5, "repo", true),
            entry(7, 6, "a.txt", false),
        ]);
        let stats = index.apply_changes(
            vec![super::NtfsRecord {
                file_ref: 6,
                parent_ref: 5,
                name: "repo".to_string(),
                attributes: 16,
                usn: 11,
                timestamp: 0,
                reason: USN_REASON_FILE_DELETE,
            }],
            11,
            1,
        );
        assert_eq!(stats.deleted, 2, "目录连同子文件一起删");
        assert_eq!(index.total_entries(), 1);
        assert!(index.search("a.txt".to_string(), None).results.is_empty());
    }
}
