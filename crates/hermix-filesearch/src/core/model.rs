use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    Mutex,
};

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
    /// 卷序列号（`GetVolumeInformationW` 的 serial）。
    ///
    /// **为什么要有**：快照文件按卷**盘符**命名（`indexes\C.bin`），
    /// 盘符会复用——换一块盘、同一个字母指向另一块物理盘时，旧快照
    /// 会被当成新卷的索引加载，界面上就是「索引出来的数据不对」。
    /// 序列号是卷实例的身份，序列号对不上 = 这份快照不是这个卷的。
    #[serde(default)]
    pub volume_serial: u32,
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
    /// 文件字节数（MFT file record 的 `$DATA`）。目录为 0——目录体积由
    /// 父子引用累加得到。
    #[serde(default)]
    pub size: u64,
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
    /// 卷序列号：用来判定「磁盘上的快照是不是这个卷的」。
    pub serial: u32,
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
    /// 取到体积的文件数（读 file record 的 `$DATA` 成功数）。
    pub size_files: usize,
    /// 参与体积统计的文件总数。
    pub size_files_total: usize,
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
    /// **最后一次有实际变更**的同步时间。
    ///
    /// USN 同步每秒轮询一次，若把每次轮询都记成「上次同步」，
    /// 界面上就永远是「刚刚」——用户看不出索引到底有没有在保鲜。
    /// 所以这里只在真的应用了变更时更新；「刚刚检查过」是另一件事
    /// （见 `last_checked_at`）。
    pub last_sync_at: Option<u64>,
    /// 最后一次**检查**（无论有无变更）的时间。
    pub last_checked_at: Option<u64>,
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
    /// 已索引体积（索引内文件字节数之和，来自 MFT file record 的 `$DATA`）。
    pub indexed_bytes: u64,
    /// 索引建立时间（unix 毫秒）——与「上次同步」是两件事。
    pub indexed_at: Option<u64>,
    /// 建索引阶段（枚举中 / 读体积中 / 落盘中 / 就绪 / 失败）。
    pub build_phase: BuildPhase,
    /// 建索引已扫描条目数（枚举阶段 = 已读到的 MFT 记录数）。
    pub build_scanned: u64,
    /// 建索引已知总量（0 = 未知；读体积阶段才有）。
    pub build_total: u64,
    /// 建索引失败原因。
    pub build_error: Option<String>,
}

/// 建索引阶段。UI 按这个出进度文案，不靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildPhase {
    Idle = 0,
    /// 枚举 MFT 记录中。
    Enumerating = 1,
    /// 读文件体积（`$DATA`）中。
    ReadingSizes = 2,
    /// 写内存索引中。
    Writing = 3,
    /// 落盘快照中。
    Saving = 4,
    /// 已就绪。
    Ready = 5,
    /// 失败（原因见 `build_error`）。
    Failed = 6,
}

/// 建索引进度快照（给 UI 轮询）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildProgressSnapshot {
    pub phase: BuildPhase,
    pub scanned: u64,
    pub total: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
}

/// 单卷建索引进度（原子计数）。
///
/// **为什么是原子量而不是事件推送**：进度只需要回答「现在读到哪了」，
/// UI 每 2 秒轮询一次状态就够了；事件通道要多一套订阅生命周期，
/// 换不来可见收益（与 2026-10-04 笔记「独立只读状态命令」的取舍一致）。
#[derive(Debug, Default)]
pub struct BuildProgress {
    phase: AtomicU8,
    scanned: AtomicU64,
    total: AtomicU64,
    started_at: AtomicU64,
    finished_at: AtomicU64,
    error: Mutex<Option<String>>,
}

impl BuildProgress {
    pub fn set_phase(&self, phase: BuildPhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    pub fn add_scanned(&self, count: u64) {
        self.scanned.fetch_add(count, Ordering::Relaxed);
    }

    /// 换阶段时清零计数（枚举阶段数 MFT 记录，体积阶段数文件）。
    pub fn reset_scanned(&self) {
        self.scanned.store(0, Ordering::Relaxed);
    }

    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Release);
    }

    /// 新一轮建索引开始：清掉上一轮的错误与计数。
    pub fn begin(&self) {
        self.scanned.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Release);
        self.started_at.store(now_ms(), Ordering::Release);
        self.finished_at.store(0, Ordering::Release);
        if let Ok(mut error) = self.error.lock() {
            *error = None;
        }
        self.set_phase(BuildPhase::Enumerating);
    }

    pub fn fail(&self, message: String) {
        self.set_phase(BuildPhase::Failed);
        self.finished_at.store(now_ms(), Ordering::Release);
        if let Ok(mut error) = self.error.lock() {
            *error = Some(message);
        }
    }

    pub fn finish(&self) {
        self.set_phase(BuildPhase::Ready);
        self.finished_at.store(now_ms(), Ordering::Release);
    }

    pub fn snapshot(&self) -> BuildProgressSnapshot {
        let phase = match self.phase.load(Ordering::Acquire) {
            1 => BuildPhase::Enumerating,
            2 => BuildPhase::ReadingSizes,
            3 => BuildPhase::Writing,
            4 => BuildPhase::Saving,
            5 => BuildPhase::Ready,
            6 => BuildPhase::Failed,
            _ => BuildPhase::Idle,
        };
        let started_at = self.started_at.load(Ordering::Acquire);
        let finished_at = self.finished_at.load(Ordering::Acquire);
        BuildProgressSnapshot {
            phase,
            scanned: self.scanned.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Acquire),
            started_at: (started_at > 0).then_some(started_at),
            finished_at: (finished_at > 0).then_some(finished_at),
            error: self.error.lock().ok().and_then(|error| error.clone()),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NtfsRecord {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    pub attributes: u32,
    pub usn: i64,
    pub timestamp: i64,
    pub reason: u32,
    /// 文件字节数（来自 MFT file record 的 `$DATA`；USN 增量里没有，0 = 未知）。
    #[serde(default)]
    pub size: u64,
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

/// 快照格式版本。
/// - v2：条目从 `FileEntry`（含 path/volume）换成紧凑的 `IndexEntry`
///   （只有名字与父引用）——旧快照直接判版本不匹配、触发重建。
/// - v3：`IndexEntry` 增加 `size`（MFT file record 的 `$DATA` 体积）。
/// - v4：`VolumeIndexState` 增加 `volume_serial`（卷序列号）——
///   快照从别的机器/别的物理盘带过来时必须作废重建（否则「数据不对」）。
pub const SNAPSHOT_VERSION: u32 = 4;
