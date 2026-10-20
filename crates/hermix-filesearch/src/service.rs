use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use serde::Serialize;

use crate::{
    core::{
        model::{FileEntry, FilterRules, IndexStatus, SearchResponse},
        windows_ntfs,
    },
    monitor, ops,
    state::VolumeState,
};

/// 跨卷合并时的排名（与 `core::search::rank_of` 同口径）。
fn rank_file(entry: &FileEntry, query_lower: &str) -> u8 {
    let name = entry.name.to_lowercase();
    if name == query_lower {
        0
    } else if name.starts_with(query_lower) {
        1
    } else {
        2
    }
}

/// 多卷聚合状态（给 UI / stdio 协议消费）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub volumes: Vec<IndexStatus>,
    /// 建索引失败/不支持的卷及原因（如超时放弃、权限不足）。
    pub skipped: BTreeMap<String, String>,
}

/// 多卷搜索服务：FilePulse 的 SearchIndex 是单卷硬编码，这里在 wrapper 层
/// 每卷持一个 VolumeState（独立索引 + 独立快照 + 独立监听循环），搜索 fan-out 合并。
/// core 一行不改算法。
pub struct SearchService {
    data_dir: PathBuf,
    filter: RwLock<FilterRules>,
    volumes: RwLock<BTreeMap<String, Arc<VolumeState>>>,
    skipped: RwLock<BTreeMap<String, String>>,
}

impl SearchService {
    pub fn new(data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            filter: RwLock::new(FilterRules::default()),
            volumes: RwLock::new(BTreeMap::new()),
            skipped: RwLock::new(BTreeMap::new()),
        })
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn filter(&self) -> FilterRules {
        self.filter.read().map(|rules| rules.clone()).unwrap_or_default()
    }

    /// 更新过滤规则：增量期立即生效；全量生效在下次重建（避免为改规则重扫全盘）。
    pub fn set_filter(&self, rules: FilterRules) {
        if let Ok(mut guard) = self.filter.write() {
            *guard = rules.clone();
        }
        if let Ok(volumes) = self.volumes.read() {
            for state in volumes.values() {
                if let Ok(mut index) = state.index.write() {
                    index.set_filter(rules.clone());
                }
            }
        }
    }

    /// 发现全部受支持 NTFS 卷，装快照（秒级可搜）+ 起监听循环 + 缺失卷后台补建。
    /// `only` 给定时只处理指定卷（如 "C:","D:"）。
    pub fn ensure_started(self: &Arc<Self>, only: Option<Vec<String>>) {
        let volumes_info = match self.list_volumes_guarded() {
            Ok(list) => list,
            Err(error) => {
                eprintln!("[filesearch] 卷枚举失败：{error}");
                return;
            }
        };

        let mut pending: Vec<(String, Arc<VolumeState>)> = Vec::new();
        for info in volumes_info {
            if let Some(only) = &only {
                if !only.iter().any(|name| name.eq_ignore_ascii_case(&info.name)) {
                    continue;
                }
            }
            if !info.is_supported {
                self.record_skipped(&info.name, info.reason.clone().unwrap_or_else(|| "卷不受支持".to_string()));
                continue;
            }

            let filter = self.filter();
            let state = VolumeState::new();
            let has_snapshot = state.load_snapshot(&self.data_dir, &info.name, &filter);

            if let Ok(mut map) = self.volumes.write() {
                map.insert(info.name.clone(), state.clone());
            }

            monitor::start(state.clone(), self.data_dir.clone(), filter.clone());

            if !has_snapshot {
                pending.push((info.name.clone(), state));
            }
        }

        // 串行建索引：每卷一次全盘 MFT 扫描，多卷并发会把磁盘队列占满
        // （实测 C:+F: 同时扫 → 磁盘 100%、整机卡死）。
        if !pending.is_empty() {
            let service = self.clone();
            tokio::spawn(async move {
                for (volume, state) in pending {
                    // 卷之间留间隙，让上一轮的脏页刷完再开下一个。
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    match ops::build_and_save(
                        &state,
                        service.data_dir(),
                        &volume,
                        &service.filter(),
                    )
                    .await
                    {
                        Ok(summary) => {
                            eprintln!(
                                "[filesearch] {} 建索引完成：{} 项（{} ms）",
                                volume, summary.total_entries, summary.elapsed_ms
                            );
                        }
                        Err(error) => {
                            eprintln!("[filesearch] {volume} 建索引失败：{error}");
                            service.record_skipped(&volume, error);
                        }
                    }
                }
            });
        }
    }

    /// 目录体积（MFT 视角）：秒级出结果，不走 walkdir。
    ///
    /// 体积来自 MFT file record 的 `$DATA`（见 `core::mft_size`），目录体积
    /// 按父子引用自底向上累加——与 `du` 同算法。`parent_ref` 为空表示卷根。
    pub fn disk_usage(
        &self,
        volume: &str,
        parent_ref: Option<u64>,
        limit: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let start = std::time::Instant::now();
        let volumes = self
            .volumes
            .read()
            .map_err(|_| "索引锁状态异常。".to_string())?;
        let state = volumes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(volume))
            .map(|(_, state)| state.clone())
            .ok_or_else(|| format!("卷 {volume} 尚未索引（先搜索一次即可建索引）"))?;
        let index = state
            .index
            .read()
            .map_err(|_| "索引锁状态异常。".to_string())?;
        let (nodes, total_bytes) = index.directory_usage(parent_ref, limit.unwrap_or(200).clamp(1, 2000));
        Ok(serde_json::json!({
            "volume": volume,
            "nodes": nodes,
            "totalBytes": total_bytes,
            "elapsedMs": start.elapsed().as_millis() as u64,
        }))
    }

    /// 跨卷搜索：各卷独立检索后按统一排序（精确名 > 前缀 > 其他子串，目录优先）合并。
    pub fn search(&self, query: String, limit: Option<usize>) -> SearchResponse {
        let start = std::time::Instant::now();
        let normalized = query.trim().to_lowercase();
        let per_volume_limit = limit.map(|n| n.max(1));

        let mut total_matches = 0usize;
        let mut merged: Vec<FileEntry> = Vec::new();
        if let Ok(volumes) = self.volumes.read() {
            for state in volumes.values() {
                let Ok(index) = state.index.read() else {
                    continue;
                };
                let response = index.search(query.clone(), per_volume_limit);
                total_matches += response.total_matches;
                merged.extend(response.results);
            }
        }

        if normalized.is_empty() {
            merged.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.file_ref.cmp(&b.file_ref))
            });
        } else {
            merged.sort_by(|a, b| {
                rank_file(a, &normalized)
                    .cmp(&rank_file(b, &normalized))
                    .then_with(|| b.is_dir.cmp(&a.is_dir))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.file_ref.cmp(&b.file_ref))
            });
        }

        let limit = limit.unwrap_or(500).clamp(1, 5_000);
        let mut results: Vec<FileEntry> = merged.into_iter().take(limit).collect();
        // 大小/修改时间在索引里是空的（构建期不做逐条 stat），只给最终
        // 返回的这几百条补——有界，不会变成全盘 I/O。
        crate::core::metadata::hydrate_result_metadata(&mut results);
        let returned = results.len();
        SearchResponse {
            query,
            total_matches,
            returned,
            results,
            elapsed_ms: start.elapsed().as_millis() as u64,
        }
    }

    pub fn status(&self) -> ServiceStatus {
        let mut volumes = Vec::new();
        if let Ok(map) = self.volumes.read() {
            for state in map.values() {
                let status = {
                    let Ok(index) = state.index.read() else {
                        continue;
                    };
                    index.status()
                };
                if let Ok(monitor) = state.monitor.read() {
                    let mut status = status;
                    status.monitor_state = monitor.monitor_state;
                    status.last_sync_at = monitor.last_sync_at;
                    status.last_sync_error = monitor.last_sync_error.clone();
                    volumes.push(status);
                } else {
                    volumes.push(status);
                }
            }
        }
        let skipped = self
            .skipped
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        ServiceStatus { volumes, skipped }
    }

    /// 进程退出前把所有脏卷快照刷盘（阻塞调用，仅在退出路径使用）。
    pub fn flush_all(&self) {
        let Ok(map) = self.volumes.read() else {
            return;
        };
        // 只刷**脏**的卷：全部重写一遍等于往磁盘写 1GB（本机 6 个卷），
        // 退出要多等几十秒，用户会以为"关不掉"。
        let dirty: Vec<(Arc<VolumeState>, PathBuf)> = map
            .values()
            .filter(|state| {
                state
                    .snapshot_dirty
                    .load(std::sync::atomic::Ordering::Acquire)
            })
            .map(|state| (state.clone(), self.data_dir.clone()))
            .collect();
        if dirty.is_empty() {
            return;
        }
        // 并行刷盘：各卷写各的，串行是 N 倍等待。
        std::thread::scope(|scope| {
            for (state, data_dir) in dirty {
                scope.spawn(move || {
                    monitor::flush_snapshot_on_exit(&state, &data_dir);
                });
            }
        });
    }

    /// 卷枚举超时看门狗。本机实测 GetVolumeInformationW 偶发永不返回
    /// （与 MFT 枚举同一类卡死卷问题），枚举线程被放弃，服务不陪葬。
    const VOLUME_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

    fn list_volumes_guarded(&self) -> Result<Vec<crate::core::model::VolumeInfo>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(windows_ntfs::list_volumes());
        });
        match rx.recv_timeout(Self::VOLUME_ENUM_TIMEOUT) {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err(format!(
                "卷枚举超时（{:?}）——存在响应异常的卷设备，跳过自动发现。",
                Self::VOLUME_ENUM_TIMEOUT
            )),
        }
    }

    fn record_skipped(&self, volume: &str, reason: String) {
        if let Ok(mut guard) = self.skipped.write() {
            guard.insert(volume.to_string(), reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_merges_across_volumes_with_rank_order() {
        // 借用真实 SearchIndex 造两个卷的命中，再验证 service 合并排序。
        // service.search 依赖 VolumeState，直接单测排序 comparator 语义即可。
        let entry = |name: &str, is_dir: bool| FileEntry {
            id: 1,
            volume: "C:".to_string(),
            file_ref: 1,
            parent_ref: 5,
            name: name.to_string(),
            path: format!(r"C:\{name}"),
            extension: None,
            size: None,
            modified: 0,
            is_dir,
            last_usn: 0,
            attributes: if is_dir { 16 } else { 0 },
        };
        let exact = entry("main", true);
        let prefix_go = entry("main.go", false);
        let prefix_rs = entry("main.rs", false);
        let other = entry("src/main.rs", false);

        let mut rows = vec![other.clone(), prefix_rs.clone(), prefix_go.clone(), exact.clone()];
        rows.sort_by(|a, b| {
            rank_file(a, "main")
                .cmp(&rank_file(b, "main"))
                .then_with(|| b.is_dir.cmp(&a.is_dir))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        assert_eq!(rows[0].name, "main");
        assert_eq!(rows[1].name, "main.go");
        assert_eq!(rows[2].name, "main.rs");
        assert_eq!(rows[3].name, "src/main.rs");
    }
}
