use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub id: u64,
    pub volume: String,
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    pub path: String,
    pub extension: Option<String>,
    pub size: Option<u64>,
    pub modified: u64,
    pub is_dir: bool,
    pub last_usn: i64,
    pub attributes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeIndexState {
    pub volume: String,
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub indexed_at: u64,
    pub last_synced_at: u64,
    pub source: String,
}

/// 索引里的一条记录 —— 等价 Everything 的 `(FRN, ParentFRN, name, attrs)`。
///
/// **不存绝对路径**：路径由 `parent_ref` 在查询时回溯出来，只给最终返回的
/// 前 N 条拼。这是 Everything / Lertaro / SwiftList 这类实现的共同点，也是
/// 内存从"GB 级"降到"百 MB 级"的关键：全盘百万文件，每条只需名字和两个
/// 引用号；存路径等于把每个目录前缀重复存一遍。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    /// 小写副本，供搜索直接 `contains`（避免每次按键对百万条做 to_lowercase）。
    pub name_lower: String,
    pub modified: u64,
    pub is_dir: bool,
    pub last_usn: i64,
    pub attributes: u32,
}

/// 搜索结果（JSON 契约不变：前端要 `path` 与 `size`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSnapshot {
    pub version: u32,
    pub volume_state: VolumeIndexState,
    pub entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    pub name: String,
    pub display_name: String,
    pub label: String,
    pub file_system: String,
    pub drive_type: String,
    pub is_supported: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildIndexSummary {
    pub volume: String,
    pub total_entries: usize,
    pub files: usize,
    pub directories: usize,
    pub elapsed_ms: u64,
    pub journal_id: u64,
    pub next_usn: i64,
    pub snapshot_saved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub volume: String,
    pub applied: usize,
    pub updated: usize,
    pub deleted: usize,
    pub elapsed_ms: u64,
    pub next_usn: i64,
    pub snapshot_saved: bool,
    pub needs_rebuild: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MonitorState {
    #[default]
    Idle,
    Watching,
    Syncing,
    NeedsRebuild,
    Error,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStatus {
    pub monitor_state: MonitorState,
    pub last_sync_at: Option<u64>,
    pub last_sync_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub query: String,
    pub total_matches: usize,
    pub returned: usize,
    pub results: Vec<FileEntry>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub root: Option<String>,
    pub volume: Option<String>,
    pub total_entries: usize,
    pub is_indexed: bool,
    pub last_scan_ms: Option<u64>,
    pub snapshot_loaded: bool,
    pub snapshot_path: Option<String>,
    pub journal_id: Option<u64>,
    pub next_usn: Option<i64>,
    pub index_source: Option<String>,
    pub needs_rebuild: bool,
    pub status_message: Option<String>,
    pub monitor_state: MonitorState,
    pub last_sync_at: Option<u64>,
    pub last_sync_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NtfsRecord {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    pub attributes: u32,
    pub usn: i64,
    pub timestamp: i64,
    pub reason: u32,
}

/// 索引过滤规则：构建期与 USN 增量期共用同一谓词（防止被排除文件经增量回流）。
/// 目录按名称精确匹配（MFT 记录无系统标志可区分）；文件按扩展名 / 隐藏+系统位过滤。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterRules {
    /// 排除的目录名（精确、不区分大小写，任意层级生效，含整棵子树）。
    pub excluded_dirs: Vec<String>,
    /// 排除的文件扩展名（小写、不带点）。
    pub excluded_extensions: Vec<String>,
    /// 跳过同时带隐藏+系统位的文件（页面文件、回收站元数据等）。
    pub skip_hidden_system: bool,
}

impl Default for FilterRules {
    fn default() -> Self {
        Self {
            excluded_dirs: ["$recycle.bin", "system volume information", "node_modules", ".git", "target"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            excluded_extensions: Vec::new(),
            skip_hidden_system: true,
        }
    }
}

impl FilterRules {
    /// FILE_ATTRIBUTE_HIDDEN (0x2) / FILE_ATTRIBUTE_SYSTEM (0x4)。
    /// 硬编码常量，避免 model 层引入 windows-sys 依赖。
    const ATTR_HIDDEN: u32 = 0x2;
    const ATTR_SYSTEM: u32 = 0x4;

    pub fn accepts(&self, name: &str, is_dir: bool, attributes: u32) -> bool {
        let lower = name.to_ascii_lowercase();
        if is_dir {
            return !self.excluded_dirs.iter().any(|dir| *dir == lower);
        }
        if matches!(lower.as_str(), "pagefile.sys" | "hiberfil.sys" | "swapfile.sys") {
            return false;
        }
        if let Some(ext) = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
            if self.excluded_extensions.iter().any(|e| *e == ext) {
                return false;
            }
        }
        if self.skip_hidden_system
            && attributes & Self::ATTR_HIDDEN != 0
            && attributes & Self::ATTR_SYSTEM != 0
        {
            return false;
        }
        true
    }
}

/// crate 内事件通道（替代 FilePulse 原版的 Tauri emit）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FileSearchEvent {
    IndexUpdated(SyncSummary),
    MonitorStatus(MonitorStatus),
}

#[derive(Debug, Clone, Default)]
pub struct ChangeStats {
    pub applied: usize,
    pub updated: usize,
    pub deleted: usize,
}

/// 快照格式版本。v2：条目从 `FileEntry`（含 path/volume）换成紧凑的
/// `IndexEntry`（只有名字与父引用）——旧快照直接判版本不匹配、触发重建。
pub const SNAPSHOT_VERSION: u32 = 2;
