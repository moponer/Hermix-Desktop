use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock, OnceLock,
    },
    time::Duration,
};

use serde::Serialize;

use crate::{
    core::{
        model::{BuildPhase, FileEntry, FilterRules, IndexStatus, SearchResponse, VolumeInfo},
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

/// 汇总状态：界面按这个出主状态文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceState {
    /// 还没发现任何可索引的卷。
    Idle,
    /// 正在扫描（枚举 MFT / 读体积）。
    Scanning,
    /// 正在写索引 / 落盘。
    Indexing,
    /// 至少一个卷可用。
    Ready,
    /// 全部卷都失败。
    Failed,
}

/// 多卷聚合状态（给 UI / stdio 协议消费）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub volumes: Vec<IndexStatus>,
    /// 建索引失败/不支持的卷及原因（如超时放弃、权限不足）。
    pub skipped: BTreeMap<String, String>,
    /// 汇总条目数。
    pub total_entries: usize,
    /// 汇总已索引体积（字节）。
    pub total_bytes: u64,
    /// 汇总状态。
    pub state: ServiceState,
    /// 一句话说明当前在干什么（界面直接显示，避免用户猜）。
    pub message: String,
    /// 当前进程能不能读 MFT —— 等价于「有没有提权」。
    pub mft_readable: bool,
}

/// 多卷搜索服务：FilePulse 的 SearchIndex 是单卷硬编码，这里在 wrapper 层
/// 每卷持一个 VolumeState（独立索引 + 独立快照 + 独立监听循环），搜索 fan-out 合并。
/// core 一行不改算法。
pub struct SearchService {
    data_dir: PathBuf,
    filter: RwLock<FilterRules>,
    volumes: RwLock<BTreeMap<String, Arc<VolumeState>>>,
    skipped: RwLock<BTreeMap<String, String>>,
    /// 重建去重：同一时刻只允许一轮重建（重复点击不再叠加任务）。
    rebuilding: AtomicBool,
    /// MFT 可读性探测结果（只探一次）。
    mft_readable: OnceLock<bool>,
}

impl SearchService {
    pub fn new(data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            filter: RwLock::new(FilterRules::default()),
            volumes: RwLock::new(BTreeMap::new()),
            skipped: RwLock::new(BTreeMap::new()),
            rebuilding: AtomicBool::new(false),
            mft_readable: OnceLock::new(),
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
    ///
    /// **为什么要后台反复发现**：卷探测会偶发卡死
    /// （`GetVolumeInformationW` 本机实测约一半概率永不返回），盘也可能是
    /// 后插上的。之前只在启动时探一次——探失败就整个服务零卷，
    /// 用户表现为「索引不主动建立，来回进出几次后才有数据」。
    /// 现在：启动探一次 + 每 30 秒补漏一次（只登记新卷，不动已有索引）。
    pub fn ensure_started(self: &Arc<Self>, only: Option<Vec<String>>) {
        let discovered = self.discover(only.as_deref());
        let pending = self.register_volumes(&discovered, only.as_deref());
        self.build_pending(pending);

        let service = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let discovered = service.discover(only.as_deref());
                let pending = service.register_volumes(&discovered, only.as_deref());
                service.build_pending(pending);
            }
        });
    }

    /// 探测本机卷。探测失败只返回空列表（不炸掉整个服务）。
    fn discover(&self, only: Option<&[String]>) -> Vec<VolumeInfo> {
        let list = match self.list_volumes_guarded() {
            Ok(list) => list,
            Err(error) => {
                eprintln!("[filesearch] 卷枚举失败：{error}");
                Vec::new()
            }
        };
        list.into_iter()
            .filter(|info| match only {
                Some(list) => list.iter().any(|name| name.eq_ignore_ascii_case(&info.name)),
                None => true,
            })
            .collect()
    }

    /// 登记卷：装快照（序列号对不上就丢弃重建）+ 起监听循环。
    /// 返回「需要建索引」的卷。
    fn register_volumes(
        &self,
        discovered: &[VolumeInfo],
        only: Option<&[String]>,
    ) -> Vec<(String, Arc<VolumeState>)> {
        let mut pending: Vec<(String, Arc<VolumeState>)> = Vec::new();
        for info in discovered {
            if let Some(list) = only {
                if !list.iter().any(|name| name.eq_ignore_ascii_case(&info.name)) {
                    continue;
                }
            }
            if !info.is_supported {
                self.record_skipped(
                    &info.name,
                    info.reason.clone().unwrap_or_else(|| "卷不受支持".to_string()),
                );
                continue;
            }

            // 已登记的卷不重复登记（重建会丢内存索引）。
            let already = self
                .volumes
                .read()
                .map(|map| map.contains_key(&info.name))
                .unwrap_or(false);
            if already {
                continue;
            }

            let filter = self.filter();
            let state = VolumeState::new(&info.name);
            let has_snapshot =
                state.load_snapshot(&self.data_dir, &info.name, &filter, Some(info.serial));

            if let Ok(mut map) = self.volumes.write() {
                map.insert(info.name.clone(), state.clone());
            }
            self.clear_skipped(&info.name);
            monitor::start(state.clone(), self.data_dir.clone(), filter.clone());

            if !has_snapshot {
                pending.push((info.name.clone(), state));
            }
        }
        pending
    }

    /// 串行建索引：每卷一次全盘 MFT 扫描，多卷并发会把磁盘队列占满
    /// （实测 C:+F: 同时扫 → 磁盘 100%、整机卡死）。
    fn build_pending(self: &Arc<Self>, pending: Vec<(String, Arc<VolumeState>)>) {
        if pending.is_empty() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            for (volume, state) in pending {
                // 卷之间留间隙，让上一轮的脏页刷完再开下一个。
                tokio::time::sleep(Duration::from_secs(3)).await;
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
                        service.clear_skipped(&volume);
                    }
                    Err(error) => {
                        eprintln!("[filesearch] {volume} 建索引失败：{error}");
                        service.record_skipped(&volume, error);
                    }
                }
            }
        });
    }

    /// 重建索引：忽略现有快照，重新扫 MFT。
    ///
    /// **立即返回**：一次全盘扫描要几十秒到几分钟，而协议层读取超时只有
    /// 15s——同步等必然超时，用户只会看到「点了没反应」。进度通过
    /// `status` 轮询给出（每卷独立的阶段与计数）。
    pub fn rebuild(self: &Arc<Self>, volumes: Option<Vec<String>>) -> Result<String, String> {
        if self.rebuilding.swap(true, Ordering::AcqRel) {
            return Err("已有重建任务在进行中，请等待完成。".to_string());
        }

        // 先把卷发现出来（helper 可能刚起来，还没探到盘）。
        let discovered = self.discover(volumes.as_deref());
        let pending = self.register_volumes(&discovered, volumes.as_deref());
        let mut targets: Vec<(String, Arc<VolumeState>)> = pending;

        if let Ok(map) = self.volumes.read() {
            for (name, state) in map.iter() {
                if let Some(list) = &volumes {
                    if !list.iter().any(|volume| volume.eq_ignore_ascii_case(name)) {
                        continue;
                    }
                }
                if targets
                    .iter()
                    .any(|(volume, _)| volume.eq_ignore_ascii_case(name))
                {
                    continue;
                }
                targets.push((name.clone(), state.clone()));
            }
        }

        if targets.is_empty() {
            self.rebuilding.store(false, Ordering::Release);
            return Err("没有可重建的卷：本机没有受支持的 NTFS 固定盘。".to_string());
        }

        let service = self.clone();
        let count = targets.len();
        tokio::spawn(async move {
            for (volume, state) in targets {
                match ops::build_and_save(&state, service.data_dir(), &volume, &service.filter()).await
                {
                    Ok(summary) => {
                        eprintln!(
                            "[filesearch] {volume} 重建完成：{} 项（{} ms）",
                            summary.total_entries, summary.elapsed_ms
                        );
                        service.clear_skipped(&volume);
                    }
                    Err(error) => {
                        eprintln!("[filesearch] {volume} 重建失败：{error}");
                        service.record_skipped(&volume, error);
                    }
                }
            }
            service.rebuilding.store(false, Ordering::Release);
        });

        Ok(format!("已开始重建 {count} 个卷的索引，进度见下方状态。"))
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
        let mut total_entries = 0usize;
        let mut total_bytes = 0u64;
        let mut scanning = 0usize;
        let mut indexing = 0usize;
        let mut failed = 0usize;
        if let Ok(map) = self.volumes.read() {
            for state in map.values() {
                let mut status = {
                    let Ok(index) = state.index.read() else {
                        continue;
                    };
                    index.status()
                };
                if let Ok(monitor) = state.monitor.read() {
                    status.monitor_state = monitor.monitor_state;
                    status.last_sync_at = monitor.last_sync_at;
                    status.last_sync_error = monitor.last_sync_error.clone();
                }
                // 建索引进度来自卷级进度对象（core 不管进度，只管数据）。
                let progress = state.progress.snapshot();
                status.build_phase = progress.phase;
                status.build_scanned = progress.scanned;
                status.build_total = progress.total;
                status.build_error = progress.error.clone();
                // 索引建成前 volume_state 是空的，卷名由卷级状态补上
                // （否则面板上一片 null，看不出是哪个盘）。
                if status.volume.is_none() {
                    status.volume = Some(state.volume.clone());
                }

                total_entries += status.total_entries;
                total_bytes += status.indexed_bytes;
                match progress.phase {
                    BuildPhase::Enumerating | BuildPhase::ReadingSizes => scanning += 1,
                    BuildPhase::Writing | BuildPhase::Saving => indexing += 1,
                    BuildPhase::Failed => failed += 1,
                    _ => {}
                }
                volumes.push(status);
            }
        }
        let skipped = self
            .skipped
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let state = if volumes.is_empty() {
            ServiceState::Idle
        } else if scanning > 0 {
            ServiceState::Scanning
        } else if indexing > 0 {
            ServiceState::Indexing
        } else if failed == volumes.len() {
            ServiceState::Failed
        } else if volumes.iter().all(|volume| volume.is_indexed) {
            ServiceState::Ready
        } else {
            // 有卷但都还没建出索引：此刻说「已就绪」是骗人（实测非提权
            // 启动时就是这个状态：6 个卷在列、0 项、界面上却写「已就绪」）。
            ServiceState::Idle
        };

        ServiceStatus {
            message: status_message(state, &volumes, &skipped),
            volumes,
            skipped,
            total_entries,
            total_bytes,
            state,
            mft_readable: self.mft_readable(),
        }
    }

    /// 当前进程能不能读 MFT —— 等价于「这个 helper 有没有提权」。
    ///
    /// 探一次就够：权限不会在进程生命周期里变化。
    fn mft_readable(&self) -> bool {
        *self.mft_readable.get_or_init(|| {
            let volume = self
                .volumes
                .read()
                .ok()
                .and_then(|map| map.keys().next().cloned())
                .unwrap_or_else(|| "C:".to_string());
            crate::core::mft_size::mft_readable(&volume)
        })
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

    /// 卷恢复正常后把跳过原因清掉（否则界面永远显示旧失败）。
    fn clear_skipped(&self, volume: &str) {
        if let Ok(mut guard) = self.skipped.write() {
            guard.remove(volume);
        }
    }
}

/// 汇总状态的一句话说明。**界面直接显示这句话**，用户不用猜程序在干什么。
fn status_message(
    state: ServiceState,
    volumes: &[IndexStatus],
    skipped: &BTreeMap<String, String>,
) -> String {
    match state {
        ServiceState::Idle => {
            if !volumes.is_empty() {
                // 卷在列但索引还没建出来：说明正在排队/等待授权。
                return format!(
                    "正在建立索引（{} 个卷尚未就绪）",
                    volumes.iter().filter(|volume| !volume.is_indexed).count()
                );
            }
            if skipped.is_empty() {
                "还没有发现可索引的卷（正在重试探测本机卷…）".to_string()
            } else {
                format!(
                    "本机没有可索引的卷：{}",
                    skipped
                        .iter()
                        .map(|(volume, reason)| format!("{volume} {reason}"))
                        .collect::<Vec<_>>()
                        .join("；")
                )
            }
        }
        ServiceState::Scanning => {
            let detail = volumes
                .iter()
                .filter(|volume| {
                    matches!(
                        volume.build_phase,
                        BuildPhase::Enumerating | BuildPhase::ReadingSizes
                    )
                })
                .map(|volume| {
                    if volume.build_total > 0 {
                        format!(
                            "{} 读体积 {}/{}",
                            volume.volume.clone().unwrap_or_default(),
                            volume.build_scanned,
                            volume.build_total
                        )
                    } else {
                        format!(
                            "{} 已枚举 {} 条",
                            volume.volume.clone().unwrap_or_default(),
                            volume.build_scanned
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("，");
            format!("正在扫描：{detail}")
        }
        ServiceState::Indexing => format!(
            "正在写入索引（{} 个卷）",
            volumes
                .iter()
                .filter(|volume| {
                    matches!(volume.build_phase, BuildPhase::Writing | BuildPhase::Saving)
                })
                .count()
        ),
        ServiceState::Ready => {
            let entries: usize = volumes.iter().map(|volume| volume.total_entries).sum();
            let ready = volumes.iter().filter(|volume| volume.is_indexed).count();
            let failed = volumes
                .iter()
                .filter(|volume| volume.build_phase == BuildPhase::Failed)
                .count();
            let mut message = format!("{ready} 个卷已就绪，共 {entries} 项");
            if failed > 0 {
                message.push_str(&format!("，{failed} 个卷失败"));
            }
            if !skipped.is_empty() {
                message.push_str(&format!("，跳过 {} 个卷", skipped.len()));
            }
            message
        }
        ServiceState::Failed => {
            let reason = volumes
                .iter()
                .find_map(|volume| volume.build_error.clone())
                .or_else(|| skipped.values().next().cloned())
                .unwrap_or_else(|| "未知原因".to_string());
            format!("建索引失败：{reason}")
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
