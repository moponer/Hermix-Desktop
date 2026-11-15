use std::{
    path::PathBuf,
    sync::{
        atomic::Ordering,
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    core::{
        model::{ChangeStats, FileSearchEvent, FilterRules, MonitorState, SyncSummary},
        storage, windows_ntfs,
    },
    ops, state::{now_ms, VolumeState},
};

const POLL_INTERVAL_SECS: u64 = 1;
const MAX_RETRY_DELAY_SECS: u64 = 30;
/// 全量快照（compaction）的窗口。平时只追加 USN 增量日志，崩溃后靠
/// 「快照 + 回放增量」恢复，所以不需要频繁重写整卷——30 分钟一次足够，
/// 退出时还有一次强制刷盘。
const SNAPSHOT_INTERVAL_MS: u64 = 30 * 60_000;
/// 增量日志攒到这么大就做一次 compaction（避免启动回放太久）。
const DELTA_COMPACT_BYTES: u64 = 32 * 1024 * 1024;
const REBUILD_RETRY_DELAY_SECS: u64 = 60;

enum MonitorReadiness {
    Idle,
    NeedsRebuild,
    Ready,
}

/// 每卷一个监听循环：USN 增量保鲜 + 节流落盘 + Journal 失效自动重建。
/// 与 FilePulse 原版的差异：去 Tauri（事件走 broadcast），NeedsRebuild 不再等手动重建。
pub fn start(state: Arc<VolumeState>, data_dir: PathBuf, filter: FilterRules) {
    tokio::spawn(async move {
        let mut retry_delay_secs = POLL_INTERVAL_SECS;

        loop {
            match readiness(&state) {
                MonitorReadiness::Idle => {
                    set_monitor_status(&state, MonitorState::Idle, None, None);
                    retry_delay_secs = POLL_INTERVAL_SECS;
                    tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
                }
                MonitorReadiness::NeedsRebuild => {
                    // Journal 失效（被清理/卷重建）→ 自动重建，重建期间旧索引继续可搜。
                    let volume = state
                        .index
                        .read()
                        .ok()
                        .and_then(|index| index.volume_state())
                        .map(|volume| volume.volume);
                    match volume {
                        Some(volume) => {
                            match ops::build_and_save(&state, &data_dir, &volume, &filter).await {
                                Ok(_) => {
                                    retry_delay_secs = POLL_INTERVAL_SECS;
                                }
                                Err(error) => {
                                    set_monitor_status(
                                        &state,
                                        MonitorState::Error,
                                        None,
                                        Some(error),
                                    );
                                    tokio::time::sleep(Duration::from_secs(REBUILD_RETRY_DELAY_SECS))
                                        .await;
                                }
                            }
                        }
                        None => {
                            tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
                        }
                    }
                }
                MonitorReadiness::Ready => match sync_once(&state, &data_dir).await {
                    Ok(summary) => {
                        retry_delay_secs = POLL_INTERVAL_SECS;
                        if !summary.needs_rebuild {
                            if let Err(error) = flush_snapshot_if_due(&state, &data_dir).await {
                                set_monitor_status(&state, MonitorState::Error, None, Some(error));
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_secs(retry_delay_secs)).await;
                        retry_delay_secs = next_retry_delay(retry_delay_secs);
                    }
                },
            }
        }
    });
}

pub async fn sync_once(
    state: &VolumeState,
    data_dir: &std::path::Path,
) -> Result<SyncSummary, String> {
    let _operation = state.operation_gate.lock().await;
    let notify_on_success = mark_syncing(state);

    let volume_state = {
        let index = state
            .index
            .read()
            .map_err(|_| sync_error(state, "搜索索引锁状态异常。".to_string()))?;
        if index.needs_rebuild() {
            return Err(sync_error(
                state,
                "USN Journal 已失效，等待自动重建。".to_string(),
            ));
        }
        index.volume_state().ok_or_else(|| {
            sync_error(state, "还没有可同步的索引，请先建立索引。".to_string())
        })?
    };

    let volume = volume_state.volume.clone();
    let previous_journal_id = volume_state.journal_id;
    let previous_next_usn = volume_state.next_usn;
    let start = Instant::now();
    let changes = match tokio::task::spawn_blocking(move || {
        windows_ntfs::read_usn_changes(volume_state)
    })
    .await
    {
        Ok(Ok(changes)) => changes,
        Ok(Err(error)) => return Err(sync_error(state, error.to_string())),
        Err(error) => return Err(sync_error(state, format!("同步任务失败：{error}"))),
    };

    if journal_requires_rebuild(
        previous_journal_id,
        previous_next_usn,
        changes.journal_id,
        changes.first_usn,
    ) {
        let message = "USN Journal 已失效或记录已被清理，即将自动重建。".to_string();
        {
            let mut index = state
                .index
                .write()
                .map_err(|_| sync_error(state, "搜索索引锁状态异常。".to_string()))?;
            index.mark_needs_rebuild(message.clone());
        }
        set_monitor_status(&state, MonitorState::NeedsRebuild, None, Some(message.clone()));
        return Ok(SyncSummary {
            volume,
            applied: 0,
            updated: 0,
            deleted: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
            next_usn: previous_next_usn,
            snapshot_saved: false,
            needs_rebuild: true,
            message: Some(message),
        });
    }

    let next_usn = changes.next_usn;
    let records = changes.records;
    let cursor_changed = next_usn != previous_next_usn;
    let synced_at = now_ms();

    // 增量只改受影响的条目：不重建路径、不重排全表、不重建小写副本
    // （旧实现每秒做这三件 O(N) 的事，百万文件机器直接被拖死）。
    let stats = if records.is_empty() {
        if cursor_changed {
            let mut index = state
                .index
                .write()
                .map_err(|_| sync_error(state, "搜索索引锁状态异常。".to_string()))?;
            index.advance_sync_cursor(next_usn, synced_at);
        }
        ChangeStats::default()
    } else {
        // 先落一份增量日志再改内存：崩溃后重启时「快照 + 回放增量」即可，
        // 不必因为丢掉几秒的变更就全盘重建（Everything 的 db+journal 同款）。
        let delta = records.clone();
        let stats = {
            let mut index = state
                .index
                .write()
                .map_err(|_| sync_error(state, "搜索索引锁状态异常。".to_string()))?;
            index.apply_changes(records, next_usn, synced_at)
        };
        if let Err(error) = storage::append_delta(data_dir, &volume, next_usn, &delta) {
            eprintln!("[filesearch] {volume} 追加增量日志失败：{error}");
        }
        stats
    };

    let changed = cursor_changed || stats.applied > 0 || stats.updated > 0 || stats.deleted > 0;
    if changed {
        state.snapshot_dirty.store(true, Ordering::Release);
    }

    mark_sync_success(state, synced_at, notify_on_success, changed);
    let message = if changed {
        "同步完成。".to_string()
    } else {
        "没有新的变化。".to_string()
    };
    let summary = SyncSummary {
        volume,
        applied: stats.applied,
        updated: stats.updated,
        deleted: stats.deleted,
        elapsed_ms: start.elapsed().as_millis() as u64,
        next_usn,
        snapshot_saved: false,
        needs_rebuild: false,
        message: Some(message),
    };

    if changed {
        let _ = state.events.send(FileSearchEvent::IndexUpdated(summary.clone()));
    }

    Ok(summary)
}

pub fn mark_index_ready(state: &VolumeState) {
    let last_sync_at = state
        .index
        .read()
        .ok()
        .and_then(|index| index.volume_state())
        .map(|volume| volume.last_synced_at);
    set_monitor_status(state, MonitorState::Watching, last_sync_at, None);
}

pub fn flush_snapshot_on_exit(state: &VolumeState, data_dir: &std::path::Path) {
    if !state.snapshot_dirty.load(Ordering::Acquire) {
        return;
    }

    let _operation = state.operation_gate.blocking_lock();
    if !state.snapshot_dirty.load(Ordering::Acquire) {
        return;
    }

    // 借引用序列化：`current_snapshot()` 会把百万级条目整体 clone 一份
    // （退出瞬间内存翻倍，容易把机器推到卡死）。`snapshot_parts` 只借切片。
    //
    // 读锁必须在下面 `index.write()` 之前释放，否则自己等自己 = 死锁：
    // 所以把写盘放进一个作用域，让 guard 在作用域结束时掉。
    let saved = {
        let Ok(index) = state.index.read() else {
            return;
        };
        let Some((volume_state, entries)) = index.snapshot_parts() else {
            return;
        };
        let volume = volume_state.volume.clone();
        (
            storage::save_snapshot_parts(data_dir, volume_state, entries),
            volume,
        )
    };
    let (result, volume) = saved;

    match result {
        Ok(path) => {
            if let Ok(mut index) = state.index.write() {
                index.set_snapshot_path(Some(path.display().to_string()));
            }
            state.snapshot_dirty.store(false, Ordering::Release);
            state.last_snapshot_saved_at.store(now_ms(), Ordering::Release);
            // 快照已包含全部条目，增量日志可以丢掉（否则下次启动会重放两遍）。
            if let Err(error) = storage::clear_delta(data_dir, &volume) {
                eprintln!("退出前清空增量日志失败：{error}");
            }
        }
        Err(error) => eprintln!("退出前保存索引快照失败：{error}"),
    }
}

async fn flush_snapshot_if_due(
    state: &Arc<VolumeState>,
    data_dir: &std::path::Path,
) -> Result<bool, String> {
    let volume = {
        let Ok(index) = state.index.read() else {
            return Ok(false);
        };
        match index.volume_state() {
            Some(volume_state) => volume_state.volume,
            None => return Ok(false),
        }
    };

    // 压缩（compaction）条件：增量日志攒够大，或距上次全量快照超过窗口。
    // 平时只追加增量，崩溃后靠「快照 + 回放增量」恢复——不必频繁重写整卷。
    let due_by_time = snapshot_is_due(
        state.snapshot_dirty.load(Ordering::Acquire),
        now_ms(),
        state.last_snapshot_saved_at.load(Ordering::Acquire),
    );
    if !due_by_time && storage::delta_size(data_dir, &volume) < DELTA_COMPACT_BYTES {
        return Ok(false);
    }

    let _operation = state.operation_gate.lock().await;
    let due_by_time = snapshot_is_due(
        state.snapshot_dirty.load(Ordering::Acquire),
        now_ms(),
        state.last_snapshot_saved_at.load(Ordering::Acquire),
    );
    if !due_by_time && storage::delta_size(data_dir, &volume) < DELTA_COMPACT_BYTES {
        return Ok(false);
    }

    // 在阻塞任务里**借引用**序列化：既不用把整份索引 clone 出来，也不占着
    // 读锁做内存复制（读锁是共享的，期间搜索照常可用）。
    let state_owned = state.clone();
    let data_dir_owned = data_dir.to_path_buf();
    let saved_path = tokio::task::spawn_blocking(move || {
        let index = state_owned
            .index
            .read()
            .map_err(|_| "搜索索引锁状态异常。".to_string())?;
        let (volume_state, entries) = index
            .snapshot_parts()
            .ok_or_else(|| "索引状态为空，无法保存快照。".to_string())?;
        storage::save_snapshot_parts(&data_dir_owned, volume_state, entries)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("保存索引快照任务失败：{error}"))??;

    {
        let mut index = state
            .index
            .write()
            .map_err(|_| "搜索索引锁状态异常。".to_string())?;
        index.set_snapshot_path(Some(saved_path.display().to_string()));
    }
    state.snapshot_dirty.store(false, Ordering::Release);
    state.last_snapshot_saved_at.store(now_ms(), Ordering::Release);
    if let Err(error) = storage::clear_delta(data_dir, &volume) {
        eprintln!("[filesearch] {volume} 清空增量日志失败：{error}");
    }
    Ok(true)
}

fn readiness(state: &VolumeState) -> MonitorReadiness {
    let Ok(index) = state.index.read() else {
        return MonitorReadiness::Idle;
    };
    if index.volume_state().is_none() {
        MonitorReadiness::Idle
    } else if index.needs_rebuild() {
        MonitorReadiness::NeedsRebuild
    } else {
        MonitorReadiness::Ready
    }
}

fn mark_syncing(state: &VolumeState) -> bool {
    let Ok(mut status) = state.monitor.write() else {
        return false;
    };
    let notify_on_success =
        status.monitor_state != MonitorState::Watching || status.last_sync_error.is_some();
    status.monitor_state = MonitorState::Syncing;
    notify_on_success
}

fn mark_sync_success(state: &VolumeState, synced_at: u64, notify: bool, changed: bool) {
    let snapshot = {
        let Ok(mut status) = state.monitor.write() else {
            return;
        };
        status.monitor_state = MonitorState::Watching;
        // **只有真的应用了变更**才算「上次同步」。USN 每秒轮询一次，
        // 若每次都刷新时间戳，界面上「上次同步」永远是「刚刚」——
        // 用户看不出索引到底有没有在保鲜。轮询时间另记 last_checked_at。
        if changed {
            status.last_sync_at = Some(synced_at);
        }
        status.last_checked_at = Some(synced_at);
        status.last_sync_error = None;
        status.clone()
    };
    if notify {
        let _ = state.events.send(FileSearchEvent::MonitorStatus(snapshot));
    }
}

fn sync_error(state: &VolumeState, error: String) -> String {
    set_monitor_status(state, MonitorState::Error, None, Some(error.clone()));
    error
}

fn set_monitor_status(
    state: &VolumeState,
    monitor_state: MonitorState,
    last_sync_at: Option<u64>,
    last_sync_error: Option<String>,
) {
    let (changed, snapshot) = {
        let Ok(mut status) = state.monitor.write() else {
            return;
        };
        let changed =
            status.monitor_state != monitor_state || status.last_sync_error != last_sync_error;
        status.monitor_state = monitor_state;
        if last_sync_at.is_some() {
            status.last_sync_at = last_sync_at;
            // 传入同步时间 = 刚刚确实检查过（建索引完成、状态切换时）。
            status.last_checked_at = last_sync_at;
        }
        status.last_sync_error = last_sync_error;
        (changed, status.clone())
    };

    if changed {
        let _ = state.events.send(FileSearchEvent::MonitorStatus(snapshot));
    }
}

fn journal_requires_rebuild(
    previous_journal_id: u64,
    previous_next_usn: i64,
    journal_id: u64,
    first_usn: i64,
) -> bool {
    journal_id != previous_journal_id || first_usn > previous_next_usn
}

fn next_retry_delay(current: u64) -> u64 {
    current.saturating_mul(2).min(MAX_RETRY_DELAY_SECS)
}

fn snapshot_is_due(dirty: bool, now: u64, last_saved_at: u64) -> bool {
    dirty && now.saturating_sub(last_saved_at) >= SNAPSHOT_INTERVAL_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_uses_exponential_backoff_with_cap() {
        let mut delay = 1;
        let mut values = Vec::new();
        for _ in 0..6 {
            values.push(delay);
            delay = next_retry_delay(delay);
        }
        assert_eq!(values, vec![1, 2, 4, 8, 16, 30]);
        assert_eq!(next_retry_delay(30), 30);
    }

    #[test]
    fn detects_invalid_or_truncated_journal() {
        assert!(journal_requires_rebuild(1, 100, 2, 0));
        assert!(journal_requires_rebuild(1, 100, 1, 101));
        assert!(!journal_requires_rebuild(1, 100, 1, 100));
    }

    #[test]
    fn snapshot_is_throttled_until_interval_elapsed() {
        assert!(!snapshot_is_due(false, SNAPSHOT_INTERVAL_MS * 2, 0));
        assert!(!snapshot_is_due(true, SNAPSHOT_INTERVAL_MS - 1, 0));
        assert!(snapshot_is_due(true, SNAPSHOT_INTERVAL_MS, 0));
        assert!(snapshot_is_due(true, SNAPSHOT_INTERVAL_MS * 2, SNAPSHOT_INTERVAL_MS));
    }

    /// 「上次同步」只在真有变更时前进：否则每秒轮询一次 → 永远「刚刚」。
    #[test]
    fn last_sync_at_only_moves_on_real_changes() {
        let state = VolumeState::new("C:");

        mark_sync_success(&state, 1_000, false, false);
        {
            let status = state.monitor.read().expect("monitor");
            assert_eq!(status.last_sync_at, None, "无变更不刷新上次同步");
            assert_eq!(status.last_checked_at, Some(1_000), "但检查时间照常推进");
        }

        mark_sync_success(&state, 2_000, false, true);
        {
            let status = state.monitor.read().expect("monitor");
            assert_eq!(status.last_sync_at, Some(2_000), "有变更才推进上次同步");
        }
    }
}
