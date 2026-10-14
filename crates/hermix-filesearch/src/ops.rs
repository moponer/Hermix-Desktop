use std::{path::Path, sync::{atomic::Ordering, Arc}, time::Duration};

use crate::{
    core::{model::{BuildIndexSummary, FilterRules}, storage, windows_ntfs},
    monitor, state::{now_ms, VolumeState},
};

/// 单卷 MFT 全量构建的超时看门狗。超时后阻塞线程被放弃（无法强杀），
/// 该卷标记跳过，绝不让一个卡死卷拖死整个索引服务。
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// 建索引 + 落快照 + 标记监听就绪。初始构建与 USN 失效后的自动重建共用此入口。
pub async fn build_and_save(
    state: &Arc<VolumeState>,
    data_dir: &Path,
    volume: &str,
    filter: &FilterRules,
) -> Result<BuildIndexSummary, String> {
    let _operation = state.operation_gate.lock().await;
    let volume_owned = volume.to_string();
    let filter_owned = filter.clone();
    let build = match tokio::time::timeout(
        BUILD_TIMEOUT,
        tokio::task::spawn_blocking(
            move || windows_ntfs::build_volume_index(volume_owned, &filter_owned),
        ),
    )
    .await
    {
        Ok(Ok(result)) => result.map_err(|error| error.to_string())?,
        Ok(Err(error)) => return Err(format!("建立索引任务失败：{error}")),
        Err(_) => return Err(format!("建立索引超时（{:?}），该卷已放弃。", BUILD_TIMEOUT)),
    };

    let mut summary = build.summary;
    let snapshot_path = storage::snapshot_path(data_dir, &build.state.volume)
        .map(|path| path.display().to_string())
        .ok();

    let snapshot = {
        let mut index = state
            .index
            .write()
            .map_err(|_| "搜索索引锁状态异常。".to_string())?;
        index.set_filter(filter.clone());
        index.replace_volume(build.state, build.entries, summary.elapsed_ms, snapshot_path);
        index
            .current_snapshot()
            .ok_or_else(|| "索引状态为空，无法保存快照。".to_string())?
    };

    let data_dir_owned = data_dir.to_path_buf();
    let saved_path = tokio::task::spawn_blocking(move || storage::save_snapshot(&data_dir_owned, &snapshot))
        .await
        .map_err(|error| format!("保存索引快照任务失败：{error}"))?
        .map_err(|error| error.to_string())?;

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
    Ok(summary)
}
