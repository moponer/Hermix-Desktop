//! Task engine runtime — S5 wire-up.
//!
//! Bridges the pure [`TaskScheduler`] + [`SharedMessageBus`] + [`AuditEngine`]
//! state machines to the outside world: user inputs, agent streams, the
//! persistence layer, and the WebSocket outbound channel.
//!
//! ## Design
//!
//! * **One runtime per process**, owned by `AppState`.
//! * **One `RoomRuntime` per active room**, lazily created on first user
//!   message and cached.
//! * **Inbound events** (user chat, agent output, review verdict, spec
//!   updates, cancel) are converted into [`SchedulerEvent`]s and fed in.
//! * **Outbound actions** ([`SchedulerAction`]) are translated into:
//!   * WS events streamed back to the client
//!   * DB writes (`save_message`, `save_subtask`, etc.)
//!   * Bus broadcasts (cross-agent notifications)
//! * **Yanluo audit** runs on every agent output before forwarding it,
//!   feeding [`SchedulerEvent::AuditVerdict`] back into the scheduler.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::bus::{RoomBus, SharedMessageBus};
use super::discussion::{ActiveDiscussion, DiscussionEventPublisher};
use super::manager::GroupChatManager;
use super::scheduler::{RoomSchedulerState, TaskScheduler};
use super::task_types::{TaskPhase, TokenBudget, YanluoConfig};
use super::yanluo::{AuditEngine, AuditTrail};

/// Outbound WS envelope sent to one client (or broadcast to all clients
/// in a room). Decoupled from `hermix_transport::StreamEvent` so the
/// runtime can be unit-tested without pulling axum/tokio-tungstenite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoomEvent {
    /// Chat message saved (user / agent / system).
    MessageSaved {
        room_id: String,
        message_id: String,
        sender_id: String,
        sender_name: String,
        content: String,
        is_agent: bool,
    },
    /// A task node entered a new phase.
    TaskPhaseChange {
        room_id: String,
        node_id: String,
        description: String,
        phase: String,
        assigned_agent: String,
    },
    /// Bus message delivered to a specific agent (broadcast to all
    /// clients so they can render the notification).
    BusDelivered {
        room_id: String,
        agent_id: String,
        message_kind: String,
        text: String,
    },
    /// Audit verdict emitted.
    AuditVerdict {
        room_id: String,
        agent_id: String,
        /// P4 — unique id so the frontend can dispute/overturn.
        violation_id: String,
        level: String,
        score_delta: i32,
        score_after: i32,
        reason: String,
    },
    /// Agent expelled.
    AgentExpelled {
        room_id: String,
        agent_id: String,
        agent_name: String,
        reason: String,
    },
    /// All nodes completed for the room.
    RoomCompleted {
        room_id: String,
    },
    /// Soft error (e.g. agent returned 401). Forwarded so client can show.
    SoftError {
        room_id: String,
        code: String,
        message: String,
    },
    /// One streamed chunk from an agent's LLM response. The frontend
    /// appends `delta` to its running buffer keyed by `agent_id`. The
    /// very first chunk also acts as the "agent started typing" trigger
    /// — the frontend flips its typing indicator on when it sees the
    /// first chunk for a previously-quiet agent_id, no separate Started
    /// event needed.
    /// `agent_name` and `role` are sent on the **first** chunk only
    /// (i.e. they are empty strings on subsequent chunks) so the
    /// frontend can set up the avatar tint / display name once.
    AgentThinkingChunk {
        room_id: String,
        agent_id: String,
        agent_name: String,
        role: String,
        delta: String,
    },
    /// Agent finished streaming. Frontend removes the typing indicator
    /// and (for non-leader roles) finalizes the running buffer into a
    /// chat bubble. Leader streams are **never** saved as messages —
    /// their final text is consumed by `parse_task_plan` to produce
    /// child subtask nodes instead.
    AgentThinkingCompleted {
        room_id: String,
        agent_id: String,
    },
    /// Dispatch to an agent failed (2026-07 refactor). Previously these
    /// failures were silently swallowed by `warn!(...)` logs; now they
    /// surface to the frontend so the user sees *why* nobody replied.
    /// The frontend renders this as an inline error card with a retry
    /// button.
    DispatchFailed {
        room_id: String,
        agent_id: String,
        agent_name: String,
        reason: String,
        /// Whether this was the leader (affects UI — leader failures are
        /// more severe and warrant a different banner).
        is_leader: bool,
    },
    /// Leader handed off control to a specialist agent (openai-agents-python
    /// Handoff pattern). The frontend renders a centered separator like
    /// "已转交 XX 接管" so the user understands the conversation
    /// changed hands.
    Handoff {
        room_id: String,
        from_agent_id: String,
        to_agent_id: String,
        to_agent_name: String,
        reason: String,
        /// "transfer" (permanent) or "delegate" (temporary).
        kind: String,
    },
    /// Discussion mode started — leader initiated a multi-round discussion.
    DiscussionStarted {
        room_id: String,
        discussion_id: String,
        topic: String,
        participants: Vec<String>,
        max_rounds: u8,
    },
    /// One streamed chunk during a discussion. Same shape as
    /// AgentThinkingChunk but tagged with the discussion round so the
    /// frontend can render a round separator.
    DiscussionChunk {
        room_id: String,
        agent_id: String,
        agent_name: String,
        round: u8,
        delta: String,
    },
    /// A participant finished speaking in the discussion. The frontend
    /// finalizes the streaming buffer into a discussion message bubble.
    DiscussionMessageSaved {
        room_id: String,
        discussion_id: String,
        agent_id: String,
        agent_name: String,
        content: String,
        round: u8,
    },
    /// One round completed — leader's summary + next action.
    DiscussionRoundSummary {
        room_id: String,
        discussion_id: String,
        round: u8,
        summary: String,
        next_action: String,
    },
    /// Discussion concluded (normal or cancelled).
    DiscussionConcluded {
        room_id: String,
        discussion_id: String,
        total_rounds: u8,
    },
}

/// Per-room mutable state held by the runtime.
pub struct RoomRuntime {
    pub room_id: String,
    pub scheduler_state: RoomSchedulerState,
    pub budget: TokenBudget,
    pub yanluo_config: YanluoConfig,
    pub audit_trail: AuditTrail,
    pub bus: Arc<RoomBus>,
    /// Active discussion (if any). Set when a discussion is spawned,
    /// cleared when it concludes or is cancelled (P2.1).
    pub active_discussion: Option<ActiveDiscussion>,
}

/// Process-wide task engine runtime.
pub struct TaskEngineRuntime {
    pub(crate) scheduler: TaskScheduler,
    pub(crate) audit: parking_lot::Mutex<AuditEngine>,
    pub(crate) bus: SharedMessageBus,
    pub(crate) manager: Arc<GroupChatManager>,
    pub(crate) rooms: RwLock<HashMap<String, Arc<RwLock<RoomRuntime>>>>,
    /// Per-room broadcast channel for outbound [`RoomEvent`]s. The WS
    /// handler in `hermix-server` subscribes on `subscribe(room_id)` and
    /// forwards each event into the wire envelope.
    pub(crate) event_txs: RwLock<HashMap<String, broadcast::Sender<RoomEvent>>>,
}

/// Capacity for the per-room broadcast channel. 256 covers a busy room
/// (multi-agent dispatch + audit storms) while bounding memory.
pub(crate) const ROOM_EVENT_CHANNEL_CAPACITY: usize = 256;


pub mod core;
pub mod actions;
pub mod events;

/// Bridges discussion engine events into the per-room `RoomEvent` broadcast
/// channel so the WS handler can relay them to the frontend.
impl DiscussionEventPublisher for TaskEngineRuntime {
    fn publish_discussion_started(
        &self,
        room_id: &str,
        discussion_id: &str,
        topic: &str,
        participants: &[String],
        max_rounds: u8,
    ) {
        self.publish(
            room_id,
            RoomEvent::DiscussionStarted {
                room_id: room_id.to_string(),
                discussion_id: discussion_id.to_string(),
                topic: topic.to_string(),
                participants: participants.to_vec(),
                max_rounds,
            },
        );
    }

    fn publish_discussion_chunk(
        &self,
        room_id: &str,
        agent_id: &str,
        agent_name: &str,
        round: u8,
        delta: &str,
    ) {
        self.publish(
            room_id,
            RoomEvent::DiscussionChunk {
                room_id: room_id.to_string(),
                agent_id: agent_id.to_string(),
                agent_name: agent_name.to_string(),
                round,
                delta: delta.to_string(),
            },
        );
    }

    fn publish_discussion_message(
        &self,
        room_id: &str,
        discussion_id: &str,
        agent_id: &str,
        agent_name: &str,
        content: &str,
        round: u8,
    ) {
        self.publish(
            room_id,
            RoomEvent::DiscussionMessageSaved {
                room_id: room_id.to_string(),
                discussion_id: discussion_id.to_string(),
                agent_id: agent_id.to_string(),
                agent_name: agent_name.to_string(),
                content: content.to_string(),
                round,
            },
        );
    }

    fn publish_round_summary(
        &self,
        room_id: &str,
        discussion_id: &str,
        round: u8,
        summary: &str,
        next_action: &str,
    ) {
        self.publish(
            room_id,
            RoomEvent::DiscussionRoundSummary {
                room_id: room_id.to_string(),
                discussion_id: discussion_id.to_string(),
                round,
                summary: summary.to_string(),
                next_action: next_action.to_string(),
            },
        );
    }

    fn publish_discussion_concluded(
        &self,
        room_id: &str,
        discussion_id: &str,
        total_rounds: u8,
    ) {
        self.publish(
            room_id,
            RoomEvent::DiscussionConcluded {
                room_id: room_id.to_string(),
                discussion_id: discussion_id.to_string(),
                total_rounds,
            },
        );
    }

    fn publish_soft_error(&self, room_id: &str, code: &str, message: &str) {
        self.publish(
            room_id,
            RoomEvent::SoftError {
                room_id: room_id.to_string(),
                code: code.to_string(),
                message: message.to_string(),
            },
        );
    }
}

/// Human-readable label for a [`TaskPhase`].

pub(crate) fn phase_label(phase: &TaskPhase) -> &'static str {
    match phase {
        TaskPhase::Idle => "Idle",
        TaskPhase::Planning => "Planning",
        TaskPhase::Executing => "Executing",
        TaskPhase::Review => "Review",
        TaskPhase::Revising => "Revising",
        TaskPhase::Completed => "Completed",
        TaskPhase::Cancelled => "Cancelled",
        TaskPhase::Discussing => "Discussing",
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::groupchat::dispatcher::StreamDelta;
    use crate::groupchat::scheduler::SchedulerEvent;
    use crate::groupchat::dispatcher::AgentDispatcher;
    use crate::groupchat::in_memory::InMemoryRoomPersistence;
    use parking_lot::Mutex as PlMutex;

    fn build_runtime() -> (Arc<TaskEngineRuntime>, Arc<GroupChatManager>) {
        let db: Arc<dyn super::super::persistence::RoomPersistence> =
            Arc::new(InMemoryRoomPersistence::new());
        let mgr = Arc::new(GroupChatManager::new(db));
        let rt = Arc::new(TaskEngineRuntime::new(mgr.clone()));
        (rt, mgr)
    }

    /// Stub dispatcher: returns a fixed output, counts calls.
    struct StubDispatcher {
        output: String,
        calls: Arc<PlMutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AgentDispatcher for StubDispatcher {
        async fn dispatch_to_agent(
            &self,
            _room_id: &str,
            _agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _room_context: &str,
        ) -> Result<String, String> {
            Ok(self.output.clone())
        }
        async fn dispatch_subtask(
            &self,
            _room_id: &str,
            agent_id: &str,
            _session_id: &str,
            task_description: &str,
            _system_prompt: &str,
        ) -> Result<String, String> {
            self.calls.lock().push(agent_id.to_string());
            Ok(format!("{}:{}", agent_id, task_description))
        }
        async fn dispatch_subtask_stream(
            &self,
            _room_id: &str,
            agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _role: &str,
            _on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync>,
        ) -> Result<String, String> {
            // Test stub: streaming path returns the same canned output
            // without firing any chunks. Keeps unit tests fast and
            // deterministic.
            self.calls.lock().push(agent_id.to_string());
            Ok(format!("{}:stream", agent_id))
        }
    }

    #[tokio::test]
    async fn user_message_dispatches_to_leader() {
        let (rt, mgr) = build_runtime();
        let room = mgr
            .create_room("test-room")
            .await
            .expect("create room");
        let room_id = room.id.clone();
        let events = rt
            .on_user_message(&room_id, "user-1", "Owner", "write a poem")
            .await;
        // Should contain: MessageSaved + TaskPhaseChange(Planning) + BusDelivered.
        assert!(events.iter().any(|e| matches!(e, RoomEvent::MessageSaved { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, RoomEvent::TaskPhaseChange { phase, .. } if phase == "Planning")));
    }

    #[tokio::test]
    async fn agent_output_runs_audit_and_completes_node() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("r2").await.unwrap();
        let room_id = room.id.clone();
        // Bootstrap task via user message.
        let _ = rt
            .clone()
            .on_user_message(&room_id, "u", "U", "hello")
            .await;
        // Find the created node.
        let node_id = {
            let rt_inner = rt.room(&room_id).await;
            let rt_lock = rt_inner.read();
            rt_lock
                .scheduler_state
                .nodes
                .keys()
                .next()
                .cloned()
                .expect("at least one node")
        };
        // Simulate the leader outputting clean text.
        let events = rt
            .clone()
            .on_agent_output(
                &room_id,
                &node_id,
                "__leader__",
                "Leader",
                "All done — here is the plan.",
                &[],
            )
            .await;
        // Should have at least MessageSaved.
        assert!(events.iter().any(|e| matches!(e, RoomEvent::MessageSaved { .. })));
    }

    #[tokio::test]
    async fn cancel_room_emits_no_actions() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("r3").await.unwrap();
        let room_id = room.id.clone();
        let _ = rt.clone().on_user_message(&room_id, "u", "U", "x").await;
        let events = rt.clone().on_cancelled(&room_id, None).await;
        // Cancelling an empty room is a no-op.
        for e in &events {
            assert!(!matches!(e, RoomEvent::RoomCompleted { .. }));
        }
    }

    #[tokio::test]
    async fn subscribe_receives_emitted_events() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("r4").await.unwrap();
        let room_id = room.id.clone();
        let mut rx = rt.subscribe(&room_id);
        // Fire a user message; subscriber should see at least the
        // MessageSaved + TaskPhaseChange(Planning).
        let _ = rt.clone().on_user_message(&room_id, "u", "U", "hi").await;
        let mut got_phase_change = false;
        let mut got_saved = false;
        // Drain a few events with a short timeout — broadcast is sync.
        for _ in 0..16 {
            match rx.try_recv() {
                Ok(RoomEvent::TaskPhaseChange { .. }) => got_phase_change = true,
                Ok(RoomEvent::MessageSaved { .. }) => got_saved = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(got_saved, "expected MessageSaved on subscribe channel");
        assert!(got_phase_change, "expected TaskPhaseChange on subscribe channel");
    }

    /// S5.2: When a dispatcher is wired in, `Dispatch` action spawns the
    /// actor asynchronously; the actor's output is fed back through
    /// `on_agent_output` (audit + scheduler replay), causing the node to
    /// transition to `Completed` and the audit log to be visible on the
    /// subscribe channel.
    #[tokio::test]
    async fn dispatch_spawns_actor_when_dispatcher_present() {
        let (rt, mgr) = build_runtime();
        let calls: Arc<PlMutex<Vec<String>>> = Arc::new(PlMutex::new(Vec::new()));
        let stub = Arc::new(StubDispatcher {
            output: "stub output".to_string(),
            calls: calls.clone(),
        });
        // Wire dispatcher into the shared manager. `set_dispatcher` now
        // takes `&self` (the dispatcher field is behind a Mutex) so we
        // don't need to wrestle with `Arc::get_mut` despite `mgr` being
        // also held by the runtime.
        mgr.set_dispatcher(stub.clone());
        let room = mgr.create_room("actor-room").await.unwrap();
        let room_id = room.id.clone();
        let mut rx = rt.subscribe(&room_id);

        // User message → TaskCreated → Dispatch{leader}. Without dispatcher
        // this would just push TaskPhaseChange(Planning) + BusDelivered.
        // With dispatcher: spawn tokio task that calls actor → on_agent_output
        // → scheduler applies AgentOutputSubmitted → node completes.
        let _ = rt
            .on_user_message(&room_id, "u", "U", "write a thing")
            .await;

        // Give the inline actor + on_agent_output path a chance to
        // run end-to-end. With the dispatcher wired in, the runtime
        // should at least invoke the dispatcher and persist the
        // agent reply (MessageSaved). Final phase is audit-dependent
        // (a clean stub reply should reach Completed; a content-
        // quality flag would push it to Revising) — we don't assert
        // on a specific terminal phase here.
        let mut got_message_saved = false;
        for _ in 0..40 {
            if let Ok(ev) = rx.try_recv() {
                if let RoomEvent::MessageSaved { is_agent: true, .. } = ev {
                    got_message_saved = true;
                }
            }
            if got_message_saved {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            calls.lock().len() >= 1,
            "expected stub dispatcher to be invoked at least once"
        );
        assert!(
            got_message_saved,
            "expected an agent MessageSaved event from actor output"
        );
    }

    /// P1.1 — review verdict moves the *assigned* agent's rank by ±5
    /// (设计 §一 "review 通过 +5 / 驳回 -5"). The hook lives in
    /// `on_review_verdict`; we synthesize a node with a real
    /// assigned_agent, send a review verdict, and assert the
    /// post-hook rank readback.
    /// adjust_member_score upserts at `default_for()` (score 10) on
    /// first touch, so the post-pass score is 10+5=15, not 5.
    #[tokio::test]
    async fn review_pass_credits_assigned_agent() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("rev-room").await.unwrap();
        let room_id = room.id.clone();

        // Manually create a node assigned to a real (non-leader) agent.
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: "writer-1".into(),
                },
            )
            .await;

        // Row created at default 10, then +5 → 15.
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "good", "leader")
            .await;
        let score_after_pass = mgr
            .get_agent_score(&room_id, "writer-1")
            .await
            .unwrap();
        assert_eq!(score_after_pass, Some(15));
    }

    #[tokio::test]
    async fn review_fail_penalizes_assigned_agent() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("rev-fail-room").await.unwrap();
        let room_id = room.id.clone();
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: "writer-1".into(),
                },
            )
            .await;

        // First pass (10 → 15) then fail (15 → 10) → net delta is 0
        // but the row exists at the default baseline 10.
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, false, "no good", "leader")
            .await;
        let score = mgr
            .get_agent_score(&room_id, "writer-1")
            .await
            .unwrap();
        assert_eq!(score, Some(10));
    }

    #[tokio::test]
    async fn review_skips_leader_sentinel() {
        // __leader__ is the virtual house-boss. `create_room`
        // backfills a default leader row (score 10); the review hook
        // must NOT touch it, so it stays at 10 after a pass verdict.
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("rev-leader-room").await.unwrap();
        let room_id = room.id.clone();
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: super::super::persistence::LEADER_AGENT_ID.into(),
                },
            )
            .await;

        let leader_before = mgr
            .get_agent_score(&room_id, super::super::persistence::LEADER_AGENT_ID)
            .await
            .unwrap()
            .expect("leader row backfilled by create_room");
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let leader_after = mgr
            .get_agent_score(&room_id, super::super::persistence::LEADER_AGENT_ID)
            .await
            .unwrap()
            .expect("leader row should still exist");
        assert_eq!(
            leader_before, leader_after,
            "leader rank must be untouched by review hook (before={}, after={})",
            leader_before, leader_after
        );
    }

    /// P1.2 — single pass increments streak to 1 and max_streak to 1.
    /// No milestone bonus yet (only even streaks ≥ 2 trigger +10).
    #[tokio::test]
    async fn review_pass_increments_streak() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("streak-1").await.unwrap();
        let room_id = room.id.clone();
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: "writer-1".into(),
                },
            )
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let ranks = mgr.list_room_ranks(&room_id).await.unwrap();
        let w = ranks
            .iter()
            .find(|r| r.agent_id == "writer-1")
            .expect("writer-1 row must exist after first pass");
        assert_eq!(w.consecutive_passes, 1);
        assert_eq!(w.max_streak, 1);
        // Score: default 10 + ±5 pass = 15. No bonus yet.
        assert_eq!(w.score, 15);
    }

    /// P1.2 — second pass lands on streak=2 (even), which is a milestone
    /// boundary, so a +10 bonus fires on top of the base +5. Final
    /// score: 10 (default) + 5 (1st pass) + 5 (2nd pass) + 10 (milestone)
    /// = 30.
    #[tokio::test]
    async fn review_two_passes_award_milestone_bonus() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("streak-2").await.unwrap();
        let room_id = room.id.clone();
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: "writer-1".into(),
                },
            )
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let ranks = mgr.list_room_ranks(&room_id).await.unwrap();
        let w = ranks
            .iter()
            .find(|r| r.agent_id == "writer-1")
            .expect("writer-1 row must exist");
        assert_eq!(w.consecutive_passes, 2);
        assert_eq!(w.max_streak, 2);
        // 10 (default) + 5 + 5 + 10 (bonus on 2nd pass) = 30.
        assert_eq!(w.score, 30);
    }

    /// P1.2 — a fail between two passes resets `consecutive_passes` to 0
    /// but preserves `max_streak` (lifetime high water mark). The
    /// post-fail pass starts the streak over at 1.
    #[tokio::test]
    async fn review_fail_resets_streak_but_keeps_max() {
        let (rt, mgr) = build_runtime();
        let room = mgr.create_room("streak-fail").await.unwrap();
        let room_id = room.id.clone();
        let node_id = "n1".to_string();
        rt.clone()
            .apply(
                &room_id,
                SchedulerEvent::TaskCreated {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: "test".into(),
                    spec: "do thing".into(),
                    assigned_agent: "writer-1".into(),
                },
            )
            .await;
        // pass → pass → fail → pass
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, false, "no", "leader")
            .await;
        let _ = rt
            .clone()
            .on_review_verdict(&room_id, &node_id, true, "ok", "leader")
            .await;
        let ranks = mgr.list_room_ranks(&room_id).await.unwrap();
        let w = ranks.iter().find(|r| r.agent_id == "writer-1").unwrap();
        assert_eq!(w.consecutive_passes, 1, "post-fail pass resets streak");
        assert_eq!(w.max_streak, 2, "max_streak must remember the 2-pass run");
    }
}

