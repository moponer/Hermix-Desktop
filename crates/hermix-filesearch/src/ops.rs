use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use crate::{
    core::{
        model::{BuildIndexSummary, BuildProgress, FilterRules},
        storage, windows_ntfs,
    },
    monitor,
    progress::BuildProgressFn,
    state::{now_ms, VolumeState},
};

/// 单卷 MFT 全量构建的超时看门狗。超时后阻塞线程被放弃（无法强杀），
/// 该卷标记跳过，绝不让一个卡死卷拖死整个索引服务。
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// 建索引 + 落快照 + 标记监听就绪。初始构建与 USN 失效后的自动重建共用此入口。
///
/// `progress` 贯穿全程：枚举 MFT → 读体积 → 建表 → 落盘。UI 轮询状态时
/// 读 `state.build_progress()`，拿到的是真实数字而不是"请稍候"。
pub async fn build_and_save(
    state: &Arc<VolumeState>,
    data_dir: &Path,
    volume: &str,
    filter: &FilterRules,
    progress: Option<BuildProgressFn>,
) -> Result<BuildIndexSummary, String> {
    let _operation = state.operation_gate.lock().await;
    let volume_owned = volume.to_string();
    let filter_owned = filter.clone();

    // 进度分发器：一份写进卷状态（UI 轮询读它），一份转给调用方。
    let tracker: BuildProgressFn = {
        let state = state.clone();
        let outer = progress.clone();
        Arc::new(move |update: BuildProgress| {
            state.set_build_progress(Some(update.clone()));
            if let Some(outer) = &outer {
                outer(update);
            }
        })
    };

    let started = std::time::Instant::now();
    let build = match tokio::time::timeout(
        BUILD_TIMEOUT,
        tokio::task::spawn_blocking({
            let tracker = tracker.clone();
            let volume_owned = volume_owned.clone();
            move || {
                windows_ntfs::build_volume_index(volume_owned, &filter_owned, Some(&tracker))
            }
        }),
    )
    .await
    {
        Ok(Ok(result)) => result.map_err(|error| {
            fail_progress(&tracker, &volume_owned, started, error.to_string());
            error.to_string()
        })?,
        Ok(Err(error)) => {
            let message = format!("建立索引任务失败：{error}");
            fail_progress(&tracker, &volume_owned, started, message.clone());
            return Err(message);
        }
        Err(_) => {
            let message = format!("建立索引超时（{BUILD_TIMEOUT:?}），该卷已放弃。");
            fail_progress(&tracker, &volume_owned, started, message.clone());
            return Err(message);
        }
    };

    let mut summary = build.summary;
    let snapshot_path = storage::snapshot_path(data_dir, &build.state.volume)
        .map(|path| path.display().to_string())
        .ok();

    {
        let mut index = state
            .index
            .write()
            .map_err(|_| "搜索索引锁状态异常。".to_string())?;
        index.set_filter(filter.clone());
        index.replace_volume(build.state, build.entries, summary.elapsed_ms, snapshot_path);
    }

    // 借引用落盘（不 clone 整份索引），与 monitor 的周期刷盘同一路径。
    tracker(BuildProgress {
        volume: volume.to_string(),
        phase: "saving".to_string(),
        entries: summary.total_entries as u64,
        bytes: 0,
        elapsed_ms: started.elapsed().as_millis() as u64,
        done: false,
        error: None,
    });
    let state_owned = state.clone();
    let data_dir_owned = data_dir.to_path_buf();
    let save_started = std::time::Instant::now();
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
    summary.save_ms = save_started.elapsed().as_millis() as u64;

    {
        let mut index = state
            .index
            .write()
            .map_err(|_| "搜索索引锁状态异常。".to_string())?;
        index.set_snapshot_path(Some(saved_path.display().to_string()));
        index.set_status_message(Some("索引已建立并保存快照。".to_string()));
    }

    state.snapshot_dirty.store(false, Ordering::Release);
    state.last_snapshot_saved_at.store(now_ms(), Ordering::Release);
    summary.snapshot_saved = true;
    monitor::mark_index_ready(state);

    // 收尾：进度置为完成（保留在状态里，UI 据此显示"本次建索引用时 X 秒"，
    // 而不是进度条突然消失）。
    tracker(BuildProgress {
        volume: volume.to_string(),
        phase: "done".to_string(),
        entries: summary.total_entries as u64,
        bytes: 0,
        elapsed_ms: started.elapsed().as_millis() as u64,
        done: true,
        error: None,
    });
    Ok(summary)
}

/// 构建失败：把失败原因写进进度，UI 立刻能显示"建索引失败：……"。
fn fail_progress(
    tracker: &BuildProgressFn,
    volume: &str,
    started: std::time::Instant,
    error: String,
) {
    tracker(BuildProgress {
        volume: volume.to_string(),
        phase: "failed".to_string(),
        entries: 0,
        bytes: 0,
        elapsed_ms: started.elapsed().as_millis() as u64,
        done: true,
        error: Some(error),
    });
}
