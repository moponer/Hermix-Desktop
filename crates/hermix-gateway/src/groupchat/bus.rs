//! SharedMessageBus — actor 间通信与中断信号
//!
//! 设计目标：
//! - **房间隔离**：每个房间有独立 Bus，跨房间不可见
//! - **多生产者单消费者**：agent 产出/用户指令都广播；agent loop 各自订阅
//! - **定向消息**：SpecUpdate / InterruptSignal 走点对点
//! - **不杀 loop 的中断**：软中断是给 agent loop 推一条 `Control::SoftInterrupt`，
//!   agent 收到后重新读最新 spec 即可，不重启 actor
//! - **零依赖**：只用 tokio mpsc，不引入外部 pubsub
//!
//! 用法：
//! ```ignore
//! let bus = SharedMessageBus::new();
//! let room = bus.create_room("room-1").await;
//! let writer_rx = room.subscribe("writer").await;
//! let reviewer_rx = room.subscribe("reviewer").await;
//! room.broadcast(BusMessage::UserInput { ... }).await?;
//! room.unicast("writer", BusMessage::SpecUpdate { ... }).await?;
//! while let Some(msg) = writer_rx.recv().await { ... }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use super::task_types::TaskPhase;

/// 消息类型（actor 间通信的载荷）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BusMessage {
    /// 用户输入（广播给房间内所有 actor）
    UserInput {
        text: String,
        sender_name: String,
    },
    /// Agent 产出（广播给其他成员，主要给 reviewer / leader）
    AgentOutput {
        from: String,
        agent_name: String,
        content: String,
        /// agent 在产出里自带的 META 摘要（已解析）
        meta: Option<AgentMeta>,
    },
    /// 任务状态变化（广播）
    TaskStateChange {
        task_id: String,
        from: TaskPhase,
        to: TaskPhase,
    },
    /// 审核结果（点对点：发到被审核者）
    ReviewVerdict {
        task_id: String,
        reviewer_id: String,
        passed: bool,
        comment: String,
    },
    /// 用户改需求（点对点：发到指定 agent 提醒重读 spec）
    SpecUpdate {
        new_spec: String,
        from_user: bool,
    },
    /// 软中断（不杀 loop，agent 收到后重新读 spec + 当前任务）
    SoftInterrupt {
        reason: String,
    },
    /// 硬中断（actor 收到后退出 loop）
    Shutdown {
        reason: String,
    },
    /// 阎罗审计通知（点对点 / 广播）
    AuditNotice {
        target: Option<String>, // None = 广播
        violation: String,
        deduction: i32,
        score_after: i32,
        severity: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMeta {
    pub summary: String,
    pub progress_pct: u8,
    pub tag: String,
}

impl BusMessage {
    pub fn user_input(text: impl Into<String>, sender_name: impl Into<String>) -> Self {
        Self::UserInput {
            text: text.into(),
            sender_name: sender_name.into(),
        }
    }

    pub fn is_control(&self) -> bool {
        matches!(self, Self::SoftInterrupt { .. } | Self::Shutdown { .. })
    }
}

/// 单个房间的 bus（每个房间一个实例）
pub struct RoomBus {
    room_id: String,
    /// 订阅者表：agent_id → sender
    subscribers: RwLock<HashMap<String, mpsc::Sender<BusMessage>>>,
    /// 每个 agent 的 channel 容量
    capacity: usize,
}

impl RoomBus {
    pub fn new(room_id: impl Into<String>, capacity: usize) -> Self {
        Self {
            room_id: room_id.into(),
            subscribers: RwLock::new(HashMap::new()),
            capacity,
        }
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    /// 订阅：返回 Receiver，actor 在自己的 task 里 select 这个 channel
    /// 同名 agent 重复订阅会替换旧 channel（防止泄漏）。
    pub async fn subscribe(&self, agent_id: impl Into<String>) -> mpsc::Receiver<BusMessage> {
        let agent_id = agent_id.into();
        let (tx, rx) = mpsc::channel(self.capacity);
        self.subscribers.write().await.insert(agent_id, tx);
        rx
    }

    /// 取消订阅（actor 退出时调用，避免 channel 满后阻塞广播）
    pub async fn unsubscribe(&self, agent_id: &str) {
        self.subscribers.write().await.remove(agent_id);
    }

    /// 广播：所有订阅者都会收到
    /// 如果某个订阅者 channel 满了，跳过它（不阻塞其他订阅者），
    /// 但会记录一次"溢出"事件（返回值里给调用方一个 hint）。
    pub async fn broadcast(&self, msg: BusMessage) -> BusSendReport {
        let subs = self.subscribers.read().await;
        let total = subs.len();
        let mut delivered = 0usize;
        let mut overflowed = Vec::new();
        for (agent_id, tx) in subs.iter() {
            match tx.try_send(msg.clone()) {
                Ok(()) => delivered += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    overflowed.push(agent_id.clone());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // 订阅者已关闭，调用方自己负责清理
                }
            }
        }
        BusSendReport {
            total,
            delivered,
            overflowed,
        }
    }

    /// 点对点：发给指定 agent
    pub async fn unicast(
        &self,
        agent_id: &str,
        msg: BusMessage,
    ) -> Result<(), BusSendError> {
        let subs = self.subscribers.read().await;
        let tx = subs
            .get(agent_id)
            .ok_or_else(|| BusSendError::NotSubscribed(agent_id.to_string()))?;
        tx.try_send(msg)
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => BusSendError::Full(agent_id.to_string()),
                mpsc::error::TrySendError::Closed(_) => BusSendError::Closed(agent_id.to_string()),
            })
    }

    pub async fn subscriber_count(&self) -> usize {
        self.subscribers.read().await.len()
    }
}

#[derive(Debug, Clone, Default)]
pub struct BusSendReport {
    pub total: usize,
    pub delivered: usize,
    pub overflowed: Vec<String>,
}

impl BusSendReport {
    pub fn all_delivered(&self) -> bool {
        self.overflowed.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusSendError {
    NotSubscribed(String),
    Full(String),
    Closed(String),
}

impl std::fmt::Display for BusSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSubscribed(id) => write!(f, "agent '{}' is not subscribed", id),
            Self::Full(id) => write!(f, "agent '{}' channel is full", id),
            Self::Closed(id) => write!(f, "agent '{}' channel is closed", id),
        }
    }
}

impl std::error::Error for BusSendError {}

/// 顶层 bus 容器：管理所有房间的 RoomBus
pub struct SharedMessageBus {
    rooms: RwLock<HashMap<String, std::sync::Arc<RoomBus>>>,
    default_capacity: usize,
}

impl SharedMessageBus {
    pub fn new(default_capacity: usize) -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            default_capacity,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(64)
    }

    /// 创建或获取房间 bus
    pub async fn get_or_create(&self, room_id: &str) -> std::sync::Arc<RoomBus> {
        if let Some(bus) = self.rooms.read().await.get(room_id) {
            return bus.clone();
        }
        let mut rooms = self.rooms.write().await;
        rooms
            .entry(room_id.to_string())
            .or_insert_with(|| {
                std::sync::Arc::new(RoomBus::new(room_id, self.default_capacity))
            })
            .clone()
    }

    pub async fn get(&self, room_id: &str) -> Option<std::sync::Arc<RoomBus>> {
        self.rooms.read().await.get(room_id).cloned()
    }

    /// 销毁房间（所有订阅者 channel 自动关闭）
    pub async fn destroy_room(&self, room_id: &str) -> bool {
        self.rooms.write().await.remove(room_id).is_some()
    }

    pub async fn room_count(&self) -> usize {
        self.rooms.read().await.len()
    }
}

/// 事件 ID 生成器（用于 audit 记录）
pub fn new_event_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> AgentMeta {
        AgentMeta {
            summary: "已写第1章".into(),
            progress_pct: 10,
            tag: "writer".into(),
        }
    }

    #[tokio::test]
    async fn broadcast_delivers_to_all_subscribers() {
        let room = RoomBus::new("r1", 16);
        let mut a = room.subscribe("a").await;
        let mut b = room.subscribe("b").await;

        let report = room
            .broadcast(BusMessage::user_input("hi", "user"))
            .await;
        assert_eq!(report.total, 2);
        assert_eq!(report.delivered, 2);
        assert!(report.all_delivered());

        let ma = a.recv().await.unwrap();
        let mb = b.recv().await.unwrap();
        assert!(matches!(ma, BusMessage::UserInput { .. }));
        assert!(matches!(mb, BusMessage::UserInput { .. }));
    }

    #[tokio::test]
    async fn unicast_only_targets_specific_agent() {
        let room = RoomBus::new("r1", 16);
        let mut writer = room.subscribe("writer").await;
        let mut reviewer = room.subscribe("reviewer").await;

        room.unicast("writer", BusMessage::SpecUpdate {
            new_spec: "改写大纲".into(),
            from_user: true,
        })
        .await
        .unwrap();

        // writer 收到
        let m = tokio::time::timeout(std::time::Duration::from_millis(50), writer.recv())
            .await
            .expect("writer should receive")
            .unwrap();
        assert!(matches!(m, BusMessage::SpecUpdate { .. }));

        // reviewer 收不到
        let m = tokio::time::timeout(std::time::Duration::from_millis(20), reviewer.recv()).await;
        assert!(m.is_err(), "reviewer should NOT receive unicast");
    }

    #[tokio::test]
    async fn unsubscribe_prevents_delivery() {
        let room = RoomBus::new("r1", 16);
        let mut a = room.subscribe("a").await;
        let b_rx = room.subscribe("b").await;
        assert_eq!(room.subscriber_count().await, 2);
        drop(b_rx); // receiver 端 drop 不会自动清理 sender
        // sender 仍存在 —— 必须显式 unsubscribe
        assert_eq!(room.subscriber_count().await, 2);

        room.unsubscribe("b").await;
        assert_eq!(room.subscriber_count().await, 1);

        let report = room.broadcast(BusMessage::user_input("hi", "user")).await;
        assert_eq!(report.total, 1);
        assert_eq!(report.delivered, 1);

        let m = tokio::time::timeout(std::time::Duration::from_millis(50), a.recv())
            .await
            .expect("a should receive")
            .unwrap();
        assert!(matches!(m, BusMessage::UserInput { .. }));
    }

    #[tokio::test]
    async fn room_isolation_no_cross_room_leak() {
        let top = SharedMessageBus::new(16);
        let r1 = top.get_or_create("room-1").await;
        let r2 = top.get_or_create("room-2").await;
        let mut r1_writer = r1.subscribe("writer").await;

        r2.broadcast(BusMessage::user_input("from r2", "user"))
            .await;

        // r1 的 writer 不应该收到 r2 的消息
        let m = tokio::time::timeout(std::time::Duration::from_millis(30), r1_writer.recv()).await;
        assert!(m.is_err(), "room-1 must not receive room-2 broadcasts");
    }

    #[tokio::test]
    async fn overflow_reports_without_blocking() {
        // capacity=1, 第一个广播后订阅者没消费
        let room = RoomBus::new("r1", 1);
        let _rx = room.subscribe("slow").await;

        let r1 = room.broadcast(BusMessage::user_input("first", "u")).await;
        assert!(r1.all_delivered());
        let r2 = room.broadcast(BusMessage::user_input("second", "u")).await;
        assert_eq!(r2.overflowed, vec!["slow".to_string()]);
    }

    #[tokio::test]
    async fn soft_interrupt_does_not_kill_loop() {
        // 软中断走 unicast，receiver 还能继续收后续消息
        let room = RoomBus::new("r1", 16);
        let mut writer = room.subscribe("writer").await;

        room.unicast("writer", BusMessage::SoftInterrupt {
            reason: "用户改了 spec".into(),
        })
        .await
        .unwrap();

        let m = writer.recv().await.unwrap();
        assert!(matches!(m, BusMessage::SoftInterrupt { .. }));
        assert!(m.is_control());
        // receiver 还能继续接收下一条
        room.unicast("writer", BusMessage::user_input("继续", "user"))
            .await
            .unwrap();
        let next = writer.recv().await.unwrap();
        assert!(matches!(next, BusMessage::UserInput { .. }));
    }

    #[tokio::test]
    async fn audit_notice_broadcast_to_all() {
        let room = RoomBus::new("r1", 16);
        let mut a = room.subscribe("a").await;
        let mut b = room.subscribe("b").await;

        room.broadcast(BusMessage::AuditNotice {
            target: None,
            violation: "敷衍".into(),
            deduction: -10,
            score_after: 90,
            severity: "severe".into(),
        })
        .await;

        let ma = a.recv().await.unwrap();
        let mb = b.recv().await.unwrap();
        assert!(matches!(ma, BusMessage::AuditNotice { .. }));
        assert!(matches!(mb, BusMessage::AuditNotice { .. }));
    }

    #[tokio::test]
    async fn destroy_room_closes_all_channels() {
        let top = SharedMessageBus::new(8);
        let r1 = top.get_or_create("r1").await;
        let mut a = r1.subscribe("a").await;
        assert_eq!(r1.subscriber_count().await, 1);

        // 先把外部 Arc 引用 drop，destroy 才能真正 drop RoomBus
        top.destroy_room("r1").await;
        assert_eq!(top.room_count().await, 0);
        drop(r1); // 释放最后一个 Arc 引用

        // 所有 sender 都被 drop，channel 关闭
        let m = tokio::time::timeout(std::time::Duration::from_millis(100), a.recv()).await;
        assert!(m.is_ok(), "recv should resolve after room destroyed");
        assert!(m.unwrap().is_none(), "channel should be closed");
    }

    #[tokio::test]
    async fn unicast_to_nonexistent_agent_errors() {
        let room = RoomBus::new("r1", 16);
        let err = room
            .unicast("ghost", BusMessage::user_input("hi", "u"))
            .await
            .unwrap_err();
        assert_eq!(err, BusSendError::NotSubscribed("ghost".to_string()));
    }

    #[tokio::test]
    async fn repeated_subscribe_replaces_old_channel() {
        let room = RoomBus::new("r1", 16);
        let old_rx = room.subscribe("agent").await;
        let mut new_rx = room.subscribe("agent").await; // 替换
        drop(old_rx); // 旧 channel 关闭

        room.unicast("agent", BusMessage::user_input("hi", "u"))
            .await
            .unwrap();

        // 新 channel 应该收到
        let m = tokio::time::timeout(std::time::Duration::from_millis(30), new_rx.recv())
            .await
            .expect("new channel should receive")
            .unwrap();
        assert!(matches!(m, BusMessage::UserInput { .. }));
    }

    #[tokio::test]
    async fn message_helpers_and_meta() {
        let m = BusMessage::user_input("hi", "你");
        match m {
            BusMessage::UserInput { text, sender_name } => {
                assert_eq!(text, "hi");
                assert_eq!(sender_name, "你");
            }
            _ => panic!("wrong variant"),
        }

        let meta = meta();
        let m = BusMessage::AgentOutput {
            from: "writer".into(),
            agent_name: "作家".into(),
            content: "第1章...".into(),
            meta: Some(meta),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("agent_output"));
        let back: BusMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, BusMessage::AgentOutput { .. }));
    }
}
