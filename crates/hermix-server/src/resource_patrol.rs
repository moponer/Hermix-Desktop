//! 资源水位巡检（总管方案 P0/P0.5）。
//!
//! Windows 没有 per-process 磁盘配额（FSRM 仅 Server SKU），磁盘治理走
//! 「scratch 白名单目录 + host 侧体积统计 + C 盘水位」三道闸（
//! docs/notes/proposed/feature/2026-10-24-ali-chief-orchestrator.md D5）。
//! 本模块是三道闸的常驻巡检：
//!
//! - 每 60s 检查 C 盘剩余水位（默认 10 GiB）与已注册 scratch 目录配额；
//! - 越限/恢复的**边沿**往 `OrchestrationEventBus` 发 `ResourceAlert`
//!   （稳态不刷屏；WS 桥会把事件推到桌面端）；
//! - [`ResourcePatrol::dispatch_blocked`] 是停派门：成员会话 / 外部
//!   worker 派单前必查，true 即拒发新工单。
//!
//! 目录体积统计是阻塞遍历，放 `spawn_blocking`；scratch 受配额约束，
//! 遍历量有上界（这正是配额的第二重意义）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermix_core::resource_governor::{
    check_dir_quota, check_watermark, QuotaVerdict, DEFAULT_C_DRIVE_MIN_FREE_BYTES,
};

use crate::orchestration_bus::{OrchestrationEvent, OrchestrationEventBus};

/// 巡检周期。
const PATROL_INTERVAL: Duration = Duration::from_secs(60);

/// 注册进巡检的 scratch 目录（worker 落位时由派单侧注册）。
#[derive(Debug, Clone)]
pub struct ScratchDir {
    pub path: PathBuf,
    pub cap_bytes: u64,
}

/// 一次巡检的结论快照。
#[derive(Debug, Clone, Default)]
pub struct PatrolSnapshot {
    /// C 盘剩余空间低于水位时为 `Some(剩余字节)`。
    pub c_drive_low_free_bytes: Option<u64>,
    /// 超配额的 scratch 目录 `(路径, 已用, 上限)`。
    pub over_quota: Vec<(PathBuf, u64, u64)>,
}

impl PatrolSnapshot {
    /// 停派门：任一闸门越限即 true。
    pub fn dispatch_blocked(&self) -> bool {
        self.c_drive_low_free_bytes.is_some() || !self.over_quota.is_empty()
    }
}

struct PatrolInner {
    scratches: Vec<ScratchDir>,
    snapshot: PatrolSnapshot,
    /// 上一拍是否处于越限（边沿触发告警）。
    breaching: bool,
}

/// 常驻资源巡检句柄。`Clone` 便宜（Arc 内核）。
#[derive(Clone)]
pub struct ResourcePatrol {
    inner: Arc<Mutex<PatrolInner>>,
    bus: OrchestrationEventBus,
    c_root: PathBuf,
    min_free_bytes: u64,
}

impl ResourcePatrol {
    /// 新建巡检（不启动循环；`run` / `tick_once` 驱动）。
    /// `c_root` 是被监护的卷根路径，`min_free_bytes` 是水位线。
    pub fn new(bus: OrchestrationEventBus, c_root: PathBuf, min_free_bytes: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PatrolInner {
                scratches: Vec::new(),
                snapshot: PatrolSnapshot::default(),
                breaching: false,
            })),
            bus,
            c_root,
            min_free_bytes,
        }
    }

    /// 生产构造：C 盘 + 默认水位。
    pub fn default_c_drive(bus: OrchestrationEventBus) -> Self {
        Self::new(bus, PathBuf::from(r"C:\"), DEFAULT_C_DRIVE_MIN_FREE_BYTES)
    }

    /// 注册一个 scratch 目录到配额巡检。
    pub fn register_scratch(&self, dir: ScratchDir) {
        self.inner
            .lock()
            .expect("resource patrol lock poisoned")
            .scratches
            .push(dir);
    }

    /// 当前停派门状态（派单侧在发工单前查询）。
    pub fn dispatch_blocked(&self) -> bool {
        self.inner
            .lock()
            .expect("resource patrol lock poisoned")
            .snapshot
            .dispatch_blocked()
    }

    /// 最新巡检快照。
    pub fn snapshot(&self) -> PatrolSnapshot {
        self.inner
            .lock()
            .expect("resource patrol lock poisoned")
            .snapshot
            .clone()
    }

    /// 常驻循环。测试不启它，用 [`Self::tick_once`] 手动驱动。
    pub async fn run(&self) {
        let mut tick = tokio::time::interval(PATROL_INTERVAL);
        loop {
            tick.tick().await;
            self.tick_once().await;
        }
    }

    /// 单次巡检：统计 + 判定 + 边沿告警 + 更新停派门。
    pub async fn tick_once(&self) {
        let scratches: Vec<ScratchDir> = self
            .inner
            .lock()
            .expect("resource patrol lock poisoned")
            .scratches
            .clone();
        let c_root = self.c_root.clone();
        let min_free = self.min_free_bytes;

        // 阻塞遍历放 spawn_blocking：大 scratch 的体积统计可能秒级。
        let snapshot = tokio::task::spawn_blocking(move || {
            scan(&c_root, min_free, &scratches)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "resource patrol scan panicked");
            PatrolSnapshot::default()
        });

        let (breaching_now, transition) = {
            let mut inner = self.inner.lock().expect("resource patrol lock poisoned");
            let breaching_now = snapshot.dispatch_blocked();
            let transition = breaching_now != inner.breaching;
            inner.breaching = breaching_now;
            inner.snapshot = snapshot;
            (breaching_now, transition)
        };

        if transition {
            if breaching_now {
                tracing::warn!(
                    c_drive_low_free_bytes = ?self.snapshot().c_drive_low_free_bytes,
                    over_quota = ?self.snapshot().over_quota.len(),
                    "资源闸门越限：停派新工单（ResourceAlert 已上总线）"
                );
                self.bus.emit(OrchestrationEvent::ResourceAlert {
                    kind: "resource_breach".to_string(),
                    message: self.describe_breach(),
                });
            } else {
                tracing::info!("资源闸门恢复：恢复派单");
                self.bus.emit(OrchestrationEvent::ResourceAlert {
                    kind: "resource_recovered".to_string(),
                    message: "磁盘水位与 scratch 配额全部恢复，重新放行派单".to_string(),
                });
            }
        }
    }

    fn describe_breach(&self) -> String {
        let snap = self.snapshot();
        let mut parts = Vec::new();
        if let Some(free) = snap.c_drive_low_free_bytes {
            parts.push(format!(
                "C 盘剩余 {} GiB 低于水位线 {} GiB",
                free / 1024 / 1024 / 1024,
                self.min_free_bytes / 1024 / 1024 / 1024
            ));
        }
        for (path, used, cap) in &snap.over_quota {
            parts.push(format!(
                "scratch {} 用量 {} GiB 超配额 {} GiB",
                path.display(),
                used / 1024 / 1024 / 1024,
                cap / 1024 / 1024 / 1024
            ));
        }
        if parts.is_empty() {
            "资源越限".to_string()
        } else {
            parts.join("；")
        }
    }
}

/// 纯扫描：水位 + 逐目录配额。任何单项失败按"该项无结论"处理，
/// 不让单目录权限问题拖垮整次巡检。
fn scan(c_root: &Path, min_free_bytes: u64, scratches: &[ScratchDir]) -> PatrolSnapshot {
    let c_drive_low_free_bytes = match check_watermark(c_root, min_free_bytes) {
        Ok(hermix_core::resource_governor::WatermarkVerdict::Low { free_bytes, .. }) => {
            Some(free_bytes)
        }
        _ => None,
    };
    let mut over_quota = Vec::new();
    for s in scratches {
        if let QuotaVerdict::Exceeded { used_bytes, cap_bytes } =
            check_dir_quota(&s.path, s.cap_bytes)
        {
            over_quota.push((s.path.clone(), used_bytes, cap_bytes));
        }
    }
    PatrolSnapshot { c_drive_low_free_bytes, over_quota }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration_bus::{OrchestrationEvent, OrchestrationEventBus};

    fn patrol_with(c_root: &str, min_free: u64) -> (ResourcePatrol, OrchestrationEventBus) {
        let bus = OrchestrationEventBus::new();
        let patrol = ResourcePatrol::new(bus.clone(), PathBuf::from(c_root), min_free);
        (patrol, bus)
    }

    #[tokio::test]
    async fn scratch_quota_breach_blocks_dispatch_and_alerts_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("blob.bin"), vec![0u8; 4096]).expect("write");
        // 水位线 0（不可能越限），只测配额闸。
        let (patrol, bus) = patrol_with(tmp.path().to_str().unwrap(), 0);
        patrol.register_scratch(ScratchDir {
            path: tmp.path().join("scratch"),
            cap_bytes: 0, // 任何字节都超
        });
        std::fs::create_dir_all(tmp.path().join("scratch")).expect("mkdir");
        std::fs::write(tmp.path().join("scratch").join("a.bin"), vec![0u8; 1024]).expect("write");

        let mut rx = bus.subscribe();
        patrol.tick_once().await;
        assert!(patrol.dispatch_blocked(), "超配额应停派");
        let evt = rx.try_recv().expect("越限边沿应发 ResourceAlert");
        match evt {
            OrchestrationEvent::ResourceAlert { kind, .. } => {
                assert_eq!(kind, "resource_breach")
            }
            other => panic!("应为 ResourceAlert，得到 {other:?}"),
        }

        // 稳态第二拍：不刷屏。
        patrol.tick_once().await;
        assert!(rx.try_recv().is_err(), "稳态不应重复告警");
    }

    #[tokio::test]
    async fn healthy_state_does_not_alert() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (patrol, bus) = patrol_with(tmp.path().to_str().unwrap(), 0);
        let mut rx = bus.subscribe();
        patrol.tick_once().await;
        assert!(!patrol.dispatch_blocked());
        assert!(rx.try_recv().is_err(), "健康状态不应告警");
    }

    #[tokio::test]
    async fn watermark_low_blocks_dispatch() {
        // 水位线 u64::MAX：任何卷都低于水位。
        let tmp = tempfile::tempdir().expect("tempdir");
        let (patrol, _bus) = patrol_with(tmp.path().to_str().unwrap(), u64::MAX);
        patrol.tick_once().await;
        assert!(patrol.dispatch_blocked(), "水位越限应停派");
    }
}
