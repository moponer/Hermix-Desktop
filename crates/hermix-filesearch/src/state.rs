use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, RwLock,
    },
};

use tokio::sync::{broadcast, Mutex};

use crate::core::{
    index::SearchIndex,
    model::{BuildProgress, FileSearchEvent, FilterRules, MonitorState, MonitorStatus},
    storage,
};

/// 单卷运行时状态。源自 FilePulse 原版 AppState（commands.rs），去 Tauri 化：
/// Tauri 的 State/AppHandle 换成 Arc 直传，前端事件推送换成 broadcast 通道。
#[derive(Debug)]
pub struct VolumeState {
    /// 卷名（如 "C:"）。
    ///
    /// **为什么单独存一份**：索引建成前 `volume_state` 是空的，
    /// 状态面板会显示不出这是哪个卷（实测状态里 volume 全是 null）。
    pub(crate) volume: String,
    pub(crate) index: RwLock<SearchIndex>,
    pub(crate) operation_gate: Mutex<()>,
    pub(crate) monitor: RwLock<MonitorStatus>,
    pub(crate) snapshot_dirty: AtomicBool,
    pub(crate) last_snapshot_saved_at: AtomicU64,
    pub(crate) events: broadcast::Sender<FileSearchEvent>,
    /// 建索引进度（原子计数，供状态面板轮询）。
    pub(crate) progress: Arc<BuildProgress>,
}

impl VolumeState {
    pub fn new(volume: &str) -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        Arc::new(Self {
            volume: volume.to_string(),
            index: RwLock::new(SearchIndex::default()),
            operation_gate: Mutex::new(()),
            monitor: RwLock::new(MonitorStatus::default()),
            snapshot_dirty: AtomicBool::new(false),
            last_snapshot_saved_at: AtomicU64::new(0),
            events,
            progress: Arc::new(BuildProgress::default()),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FileSearchEvent> {
        self.events.subscribe()
    }

    /// 启动时恢复该卷快照（秒级可搜）。快照本身在构建期已过滤，直接整卷装载。
    ///
    /// `expected_serial` 是本机该卷当前的序列号：快照里的序列号对不上，
    /// 说明这份快照**不是这个卷的**（换过盘 / 从别的机器带过来的），
    /// 必须丢弃重建——否则界面上就是「索引出来的数据不对」。
    /// 返回值：这个卷是否成功装载了快照。
    pub fn load_snapshot(
        &self,
        data_dir: &Path,
        volume: &str,
        filter: &FilterRules,
        expected_serial: Option<u32>,
    ) -> bool {
        let Ok(mut index) = self.index.write() else {
            return false;
        };
        index.set_filter(filter.clone());
        match storage::load_snapshot(data_dir, volume) {
            Ok(Some(loaded)) => {
                let snapshot_next_usn = loaded.snapshot.volume_state.next_usn;
                let mut last_sync_at = Some(loaded.snapshot.volume_state.last_synced_at);
                if let Some(expected) = expected_serial {
                    if expected != 0
                        && loaded.snapshot.volume_state.volume_serial != 0
                        && loaded.snapshot.volume_state.volume_serial != expected
                    {
                        eprintln!(
                            "[filesearch] {volume} 快照序列号 {} 与本机 {} 不符，丢弃并重建。",
                            loaded.snapshot.volume_state.volume_serial, expected
                        );
                        return false;
                    }
                }
                index.replace_snapshot(loaded.snapshot, loaded.path.display().to_string());

                // 回放增量日志（等价 Everything 的「加载 db + 重放 journal」）。
                // 只回放比快照更新的批次——压缩后日志会被清空，剩下的都是
                // 快照之后发生的变更。
                if let Ok(batches) = storage::load_deltas(data_dir, volume) {
                    for (next_usn, records) in batches {
                        if records.is_empty() || next_usn <= snapshot_next_usn {
                            continue;
                        }
                        index.apply_changes(records, next_usn, 0);
                    }
                    if let Some(state) = index.volume_state() {
                        last_sync_at = Some(state.last_synced_at.max(
                            last_sync_at.unwrap_or(0),
                        ));
                    }
                }
                if let Ok(mut status) = self.monitor.write() {
                    *status = MonitorStatus {
                        monitor_state: MonitorState::Watching,
                        last_sync_at,
                        last_checked_at: last_sync_at,
                        last_sync_error: None,
                    };
                }
                let _ = self
                    .events
                    .send(FileSearchEvent::MonitorStatus(MonitorStatus {
                        monitor_state: MonitorState::Watching,
                        last_sync_at,
                        last_checked_at: last_sync_at,
                        last_sync_error: None,
                    }));
                true
            }
            _ => false,
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{IndexEntry, IndexSnapshot, VolumeIndexState, SNAPSHOT_VERSION};

    fn snapshot(volume_serial: u32) -> IndexSnapshot {
        IndexSnapshot {
            version: SNAPSHOT_VERSION,
            volume_state: VolumeIndexState {
                volume: "C:".to_string(),
                volume_serial,
                journal_id: 1,
                first_usn: 0,
                next_usn: 0,
                indexed_at: 0,
                last_synced_at: 0,
                source: "MFT".to_string(),
            },
            entries: vec![IndexEntry {
                file_ref: 5,
                parent_ref: 5,
                name: String::new(),
                name_lower: String::new(),
                modified: 0,
                is_dir: true,
                last_usn: 0,
                attributes: 16,
                size: 0,
            }],
        }
    }

    /// 快照按**盘符**命名，盘符会复用：换盘 / 从别的机器带过来的快照
    /// 必须靠序列号认出来并丢弃，否则界面上就是「索引出来的数据不对」。
    #[test]
    fn snapshot_from_another_volume_instance_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        storage::save_snapshot(dir.path(), &snapshot(1111)).expect("save");

        let filter = FilterRules::default();
        let same = VolumeState::new("C:");
        assert!(
            same.load_snapshot(dir.path(), "C:", &filter, Some(1111)),
            "序列号一致 → 正常装载"
        );

        let other = VolumeState::new("C:");
        assert!(
            !other.load_snapshot(dir.path(), "C:", &filter, Some(2222)),
            "序列号不一致 → 丢弃并重建"
        );
    }
}
