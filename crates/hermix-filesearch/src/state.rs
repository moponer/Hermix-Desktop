use std::{
    path::Path,
    sync::{atomic::{AtomicBool, AtomicU64}, Arc, RwLock},
};

use tokio::sync::{broadcast, Mutex};

use crate::core::{
    index::SearchIndex,
    model::{FileSearchEvent, FilterRules, MonitorState, MonitorStatus},
    storage,
};

/// 单卷运行时状态。源自 FilePulse 原版 AppState（commands.rs），去 Tauri 化：
/// Tauri 的 State/AppHandle 换成 Arc 直传，前端事件推送换成 broadcast 通道。
#[derive(Debug)]
pub struct VolumeState {
    pub(crate) index: RwLock<SearchIndex>,
    pub(crate) operation_gate: Mutex<()>,
    pub(crate) monitor: RwLock<MonitorStatus>,
    pub(crate) snapshot_dirty: AtomicBool,
    pub(crate) last_snapshot_saved_at: AtomicU64,
    pub(crate) events: broadcast::Sender<FileSearchEvent>,
}

impl VolumeState {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        Arc::new(Self {
            index: RwLock::new(SearchIndex::default()),
            operation_gate: Mutex::new(()),
            monitor: RwLock::new(MonitorStatus::default()),
            snapshot_dirty: AtomicBool::new(false),
            last_snapshot_saved_at: AtomicU64::new(0),
            events,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FileSearchEvent> {
        self.events.subscribe()
    }

    /// 启动时恢复该卷快照（秒级可搜）。快照本身在构建期已过滤，直接整卷装载。
    pub fn load_snapshot(&self, data_dir: &Path, volume: &str, filter: &FilterRules) -> bool {
        let Ok(mut index) = self.index.write() else {
            return false;
        };
        index.set_filter(filter.clone());
        match storage::load_snapshot(data_dir, volume) {
            Ok(Some(loaded)) => {
                let last_sync_at = Some(loaded.snapshot.volume_state.last_synced_at);
                index.replace_snapshot(loaded.snapshot, loaded.path.display().to_string());
                if let Ok(mut status) = self.monitor.write() {
                    *status = MonitorStatus {
                        monitor_state: MonitorState::Watching,
                        last_sync_at,
                        last_sync_error: None,
                    };
                }
                let _ = self
                    .events
                    .send(FileSearchEvent::MonitorStatus(MonitorStatus {
                        monitor_state: MonitorState::Watching,
                        last_sync_at,
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
