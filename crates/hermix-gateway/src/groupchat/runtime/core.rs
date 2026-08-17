//! Runtime core: construction, bus, subscription, room access.
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::broadcast;

use crate::groupchat::bus::SharedMessageBus;
use crate::groupchat::manager::GroupChatManager;
use crate::groupchat::scheduler::{RoomSchedulerState, TaskScheduler};
use crate::groupchat::task_types::{TokenBudget, YanluoConfig};
use crate::groupchat::yanluo::{AuditEngine, AuditTrail};

use super::*;

impl TaskEngineRuntime {
    pub fn new(manager: Arc<GroupChatManager>) -> Self {
        Self {
            scheduler: TaskScheduler::new(),
            audit: parking_lot::Mutex::new(AuditEngine::new(YanluoConfig::default())),
            bus: SharedMessageBus::with_default_capacity(),
            manager,
            rooms: RwLock::new(HashMap::new()),
            event_txs: RwLock::new(HashMap::new()),
        }
    }

    pub fn shared_bus(&self) -> &SharedMessageBus {
        &self.bus
    }

    /// Subscribe to outbound [`RoomEvent`]s for a room. Lazily creates
    /// the broadcast channel on first subscription. Multiple subscribers
    /// (e.g. multiple WS connections watching the same room) are
    /// supported; each gets its own [`broadcast::Receiver`].
    pub fn subscribe(&self, room_id: &str) -> broadcast::Receiver<RoomEvent> {
        let tx = {
            let mut w = self.event_txs.write();
            w.entry(room_id.to_string())
                .or_insert_with(|| {
                    let (tx, _rx) = broadcast::channel(ROOM_EVENT_CHANNEL_CAPACITY);
                    tx
                })
                .clone()
        };
        tx.subscribe()
    }

    /// Publish one [`RoomEvent`] to the room's broadcast channel, if any
    /// subscriber exists. Never blocks; slow subscribers get
    /// [`broadcast::error::RecvError::Lagged`] on the next recv.
    pub(crate) fn publish(&self, room_id: &str, ev: RoomEvent) {
        if let Some(tx) = self.event_txs.read().get(room_id) {
            // Ignore SendError — there are simply no subscribers yet.
            let _ = tx.send(ev);
        }
    }

    /// Publish a batch (used by `apply` / `on_agent_output` / etc. so the
    /// call sites stay one-line).
    pub(crate) fn publish_all(&self, room_id: &str, events: &[RoomEvent]) {
        if events.is_empty() {
            return;
        }
        if let Some(tx) = self.event_txs.read().get(room_id) {
            for ev in events {
                let _ = tx.send(ev.clone());
            }
        }
    }

    /// Get-or-create the per-room runtime. Lazily constructs with
    /// defaults pulled from the manager's room settings.
    pub async fn room(&self, room_id: &str) -> Arc<RwLock<RoomRuntime>> {
        // Fast path: already constructed.
        if let Some(r) = self.rooms.read().get(room_id).cloned() {
            return r;
        }
        // Slow path: fetch the bus first (async, can't hold any
        // parking_lot guard across .await).
        let bus = self.bus.get_or_create(room_id).await;
        // Re-check under write lock to avoid double-construction.
        {
            let mut w = self.rooms.write();
            if let Some(r) = w.get(room_id).cloned() {
                return r;
            }
            let scheduler_state = RoomSchedulerState::new(room_id);
            let budget = TokenBudget {
                base: 8_000,
                performance_bonus: 0,
                consecutive_bonus: 0,
            };
            let runtime = Arc::new(RwLock::new(RoomRuntime {
                room_id: room_id.to_string(),
                scheduler_state,
                budget,
                yanluo_config: YanluoConfig::default(),
                audit_trail: AuditTrail::default(),
                bus,
                active_discussion: None,
            }));
            w.insert(room_id.to_string(), runtime.clone());
            runtime
        }
    }
}
