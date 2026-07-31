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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::agent_contract::{HandoffDirective, LeaderDecision};
use super::bus::{RoomBus, SharedMessageBus};
use super::dispatcher::StreamDelta;
use super::discussion::{
    ActiveDiscussion, DiscussionConfig, DiscussionEngine, DiscussionEventPublisher,
};
use super::handoff::{resolve_handoff, HandoffKind};
use super::leader::LeaderOrchestrator;
use super::manager::GroupChatManager;
use super::meta::parse_meta;
use super::persistence::{AgentRole, AgentRoomProfile, MessageType};
use super::scheduler::{
    RoomSchedulerState, SchedulerAction, SchedulerEvent, SubtaskSpec, TaskScheduler,
};
use super::task_types::{ReviewResult, TaskPhase, TokenBudget, ViolationLevel, ViolationRecord, YanluoConfig};
use super::yanluo::{
    AuditContext, AuditEngine, AuditTrail, ToolCallRecord,
};

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
    /// /// `agent_name` and `role` are sent on the **first** chunk only
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
    scheduler: TaskScheduler,
    audit: parking_lot::Mutex<AuditEngine>,
    bus: SharedMessageBus,
    manager: Arc<GroupChatManager>,
    rooms: RwLock<HashMap<String, Arc<RwLock<RoomRuntime>>>>,
    /// Per-room broadcast channel for outbound [`RoomEvent`]s. The WS
    /// handler in `hermix-server` subscribes on `subscribe(room_id)` and
    /// forwards each event into the wire envelope.
    event_txs: RwLock<HashMap<String, broadcast::Sender<RoomEvent>>>,
}

/// Capacity for the per-room broadcast channel. 256 covers a busy room
/// (multi-agent dispatch + audit storms) while bounding memory.
const ROOM_EVENT_CHANNEL_CAPACITY: usize = 256;

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
    fn publish(&self, room_id: &str, ev: RoomEvent) {
        if let Some(tx) = self.event_txs.read().get(room_id) {
            // Ignore SendError — there are simply no subscribers yet.
            let _ = tx.send(ev);
        }
    }

    /// Publish a batch (used by `apply` / `on_agent_output` / etc. so the
    /// call sites stay one-line).
    fn publish_all(&self, room_id: &str, events: &[RoomEvent]) {
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

    /// Apply an inbound event and stream the resulting outbound events.
    /// Caller (WS handler) is expected to forward each emitted event to
    /// the client.
    pub async fn apply(
        self: Arc<Self>,
        room_id: &str,
        event: SchedulerEvent,
    ) -> Vec<RoomEvent> {
        let room = self.room(room_id).await;
        let mut emitted = Vec::new();

        // Snapshot scheduler state, run pure scheduler, translate actions.
        let actions = {
            let mut rt = room.write();
            let actions = self.scheduler.apply_event(&mut rt.scheduler_state, event);
            actions
        };

        for action in actions {
            // Boxed: `dispatch_action` may recursively call back into
            // `apply` (leader → PlanSubmitted → child Dispatch), which
            // forms an async-fn cycle that the compiler can't statically
            // size without this.
            let mut more = Box::pin(self.clone().dispatch_action(room.clone(), action)).await;
            emitted.append(&mut more);
        }
        emitted
    }

    /// Translate one [`SchedulerAction`] into side effects + outbound events.
    /// Returns the events it generated so the caller can collect them
    /// (avoids `&mut Vec` borrowing across `.await` which would break
    /// the `Send` bound required by `tokio::spawn`).
    async fn dispatch_action(
        self: Arc<Self>,
        room: Arc<RwLock<RoomRuntime>>,
        action: SchedulerAction,
    ) -> Vec<RoomEvent> {
        let mut emitted = Vec::new();
        let room_id = {
            let rt = room.read();
            rt.room_id.clone()
        };
        match action {
            SchedulerAction::Dispatch { node_id, agent_id } => {
                let (phase_str, description) = {
                    let rt = room.read();
                    let node = rt.scheduler_state.nodes.get(&node_id).cloned();
                    (
                        node.as_ref()
                            .map(|n| phase_label(&n.phase).to_string())
                            .unwrap_or_else(|| "Planning".to_string()),
                        node.as_ref().map(|n| n.description.clone()).unwrap_or_default(),
                    )
                };
                emitted.push(RoomEvent::TaskPhaseChange {
                    room_id: room_id.clone(),
                    node_id: node_id.clone(),
                    description: description.clone(),
                    phase: phase_str,
                    assigned_agent: agent_id.clone(),
                });

                // If a dispatcher is wired in, run a streaming dispatch:
                // each LLM chunk becomes a `RoomEvent::AgentThinkingChunk`
                // (frontend appends to a typing/streaming bubble); a
                // final `AgentThinkingCompleted` flips the typing state
                // off. Without a dispatcher we keep the legacy "loud
                // dispatch" placeholder so callers still see the phase
                // transition.
                if let Some(dispatcher) = self.manager.dispatcher() {
                    let rt_self: Arc<TaskEngineRuntime> = self.clone();
                    let room_id_owned = room_id.clone();
                    let node_id_owned = node_id.clone();
                    let agent_id_owned = agent_id.clone();
                    let description_owned = description.clone();

                    // Look up agent display name + role tag from the room
                    // roster. Fall back to agent_id if not found (e.g.
                    // legacy rooms where the profile is missing).
                    let agent_name_owned: String = self
                        .manager
                        .agent_display_name(&room_id_owned, &agent_id_owned);
                    let role_owned: &'static str = if agent_id_owned == "__leader__" {
                        "leader"
                    } else {
                        "member"
                    };

                    // Resolve the agent's real session_id from their room
                    // profile. Previously this used `room_id` as the
                    // session_id, which meant every agent in a room shared
                    // one conversation history — defeating the per-agent
                    // memory isolation design.
                    let session_id_owned: String = self
                        .manager
                        .get_room_agents(&room_id_owned)
                        .await
                        .ok()
                        .and_then(|agents| {
                            agents
                                .iter()
                                .find(|a| a.agent_id == agent_id_owned)
                                .map(|a| a.session_id.clone())
                        })
                        .unwrap_or_else(|| room_id_owned.clone());

                    // H4: use the structured JSON decision prompt on the
                    // leader path so announcement / handoff /
                    // needs_user_confirmation actually populate (the legacy
                    // markdown prompt only produced regex-parseable output).
                    let system_prompt: String = if agent_id_owned == "__leader__" {
                        self.manager
                            .build_leader_decision_prompt_for(&room_id_owned, &description_owned)
                    } else {
                        String::new()
                    };

                    // Streaming chunk callback: publishes one
                    // `AgentThinkingChunk` per chunk so the WS handler can
                    // relay it as `StreamEvent::GroupChatChunk`. The first
                    // chunk for an agent carries `agent_name` + `role` so
                    // the frontend can set up the bubble/avatar; later
                    // chunks carry empty strings (the frontend already
                    // knows who they belong to).
                    let chunk_room = room_id_owned.clone();
                    let chunk_agent = agent_id_owned.clone();
                    let first_chunk_name = agent_name_owned.clone();
                    let first_chunk_role: String = role_owned.to_string();
                    let rt_for_chunks = rt_self.clone();
                    let first_emitted = Arc::new(AtomicBool::new(false));
                    let first_emitted_ref = first_emitted.clone();
                    let on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync> =
                        Box::new(move |delta: StreamDelta| {
                            let is_first = !first_emitted_ref.swap(true, Ordering::SeqCst);
                            let (name, role) = if is_first {
                                (first_chunk_name.clone(), first_chunk_role.clone())
                            } else {
                                (String::new(), String::new())
                            };
                            rt_for_chunks.publish(
                                &chunk_room,
                                RoomEvent::AgentThinkingChunk {
                                    room_id: chunk_room.clone(),
                                    agent_id: chunk_agent.clone(),
                                    agent_name: name,
                                    role,
                                    delta: delta.content,
                                },
                            );
                        });

                    let stream_outcome = dispatcher
                        .dispatch_subtask_stream(
                            &room_id_owned,
                            &agent_id_owned,
                            &session_id_owned,
                            &system_prompt,
                            &description_owned,
                            role_owned,
                            on_chunk,
                        )
                        .await;

                    // Always emit a "completed" event so the frontend
                    // clears its typing indicator, even on errors.
                    rt_self.publish(
                        &room_id_owned,
                        RoomEvent::AgentThinkingCompleted {
                            room_id: room_id_owned.clone(),
                            agent_id: agent_id_owned.clone(),
                        },
                    );

                    match stream_outcome {
                        Ok(raw_output) => {
                            if agent_id_owned == "__leader__" {
                                // ── Leader path (2026-07 refactor) ──────
                                // Parse the structured JSON decision. On
                                // success we get analysis + subtasks +
                                // announcement + handoff in one shot; on
                                // failure we fall back to the legacy regex
                                // task-plan parser.
                                let mut parsed_decision: Option<LeaderDecision> = None;
                                let needs_confirm = match LeaderOrchestrator::parse_decision(&raw_output) {
                                    Ok(decision) => {
                                        let confirm = decision.needs_user_confirmation;
                                        parsed_decision = Some(decision);
                                        confirm
                                    }
                                    Err(parse_err) => {
                                        tracing::warn!(
                                            error = %parse_err,
                                            "leader structured decision parse failed, falling back to regex"
                                        );
                                        false
                                    }
                                };

                                let (plan, announcement, handoff) = match &parsed_decision {
                                    Some(decision) => {
                                        let p = LeaderOrchestrator::decision_to_task_plan(decision);
                                        (p, decision.announcement.clone(), decision.handoff.clone())
                                    }
                                    None => {
                                        let p = LeaderOrchestrator::parse_task_plan(&raw_output);
                                        (p, None, None)
                                    }
                                };

                                // M5: `needs_user_confirmation` is parsed but
                                // not yet gated by a user-confirmation round
                                // trip. Surface it instead of silently
                                // proceeding so the leader's promise isn't a
                                // silent no-op. (Full confirmation gate
                                // deferred — needs an e2e verification
                                // window, which the user has asked to skip.)
                                if needs_confirm {
                                    tracing::warn!(
                                        room_id = %room_id_owned,
                                        "leader requested user confirmation (needs_user_confirmation); \
                                         auto-proceeding — confirmation gate not yet wired"
                                    );
                                }

                                // 1. Persist announcement (merged into
                                // the same LLM call — saves a round
                                // trip vs. the old separate call).
                                if let Some(ref ann) = announcement {
                                    if let Err(e) = rt_self
                                        .manager
                                        .set_announcement(&room_id_owned, Some(ann))
                                        .await
                                    {
                                        tracing::warn!(error = %e, "leader announcement persist failed");
                                    }
                                }

                                // 1.5. Discussion mode (P1.6): if the
                                // leader's decision includes a discussion
                                // config, spawn the discussion engine and
                                // return early — discussion and subtasks
                                // are mutually exclusive.
                                let discussion_config = parsed_decision
                                    .as_ref()
                                    .and_then(|d| d.discussion.clone());
                                if let Some(config) = discussion_config {
                                    tracing::info!(
                                        room_id = %room_id_owned,
                                        topic = %config.topic,
                                        participants = ?config.participants,
                                        "leader initiated discussion mode"
                                    );
                                    rt_self.clone().spawn_discussion(
                                        &room_id_owned,
                                        &node_id_owned,
                                        config,
                                        &raw_output,
                                    ).await;
                                    // Discussion runs in the background;
                                    // the current dispatch is done.
                                    return emitted;
                                }

                                // 2. Resolve handoff directive
                                // (openai-agents-python Handoff pattern).
                                let mut handoff_subtask: Option<SubtaskSpec> = None;
                                let mut handoff_kind: Option<HandoffKind> = None;
                                if let Some(ref directive) = handoff {
                                    let room_agents = rt_self
                                        .manager
                                        .get_room_agents(&room_id_owned)
                                        .await
                                        .unwrap_or_default();
                                    if let Some(resolved) =
                                        resolve_handoff(directive, &room_agents)
                                    {
                                        let target_name = rt_self.manager.agent_display_name(
                                            &room_id_owned,
                                            &resolved.target_agent_id,
                                        );
                                        let kind_str = match resolved.kind {
                                            HandoffKind::Transfer => "transfer",
                                            HandoffKind::Delegate => "delegate",
                                        };
                                        rt_self.publish(
                                            &room_id_owned,
                                            RoomEvent::Handoff {
                                                room_id: room_id_owned.clone(),
                                                from_agent_id: agent_id_owned.clone(),
                                                to_agent_id: resolved.target_agent_id.clone(),
                                                to_agent_name: target_name,
                                                reason: resolved.reason.clone(),
                                                kind: kind_str.to_string(),
                                            },
                                        );
                                        // The handoff task text: for Transfer
                                        // reuse the original task; for
                                        // Delegate use the delegated task.
                                        // (M7: Delegate now also synthesizes
                                        // a subtask so the target actually
                                        // receives work instead of only a
                                        // separator being shown.)
                                        let handoff_task: String = match directive {
                                            HandoffDirective::Transfer { .. } => {
                                                description_owned.clone()
                                            }
                                            HandoffDirective::Delegate { task, .. } => {
                                                task.clone()
                                            }
                                        };
                                        handoff_kind = Some(resolved.kind);
                                        handoff_subtask = Some(SubtaskSpec {
                                            node_id: format!("{}-handoff", node_id_owned),
                                            description: handoff_task,
                                            spec: String::new(),
                                            assigned_agent: resolved.target_agent_id.clone(),
                                            sort_order: 0,
                                        });
                                    }
                                }

                                // 3. Convert plan → subtask specs.
                                let mut subtasks: Vec<SubtaskSpec> = plan
                                    .subtasks
                                    .iter()
                                    .enumerate()
                                    .map(|(i, s)| SubtaskSpec {
                                        node_id: format!("{}-{}", node_id_owned, i + 1),
                                        description: s.description.clone(),
                                        spec: s.context.clone().unwrap_or_default(),
                                        assigned_agent: s.assigned_to.clone(),
                                        sort_order: i as u32,
                                    })
                                    .collect();

                                // L9: a Transfer hands the *whole*
                                // conversation to the specialist, so the
                                // planned subtasks must not also dispatch in
                                // parallel. (Delegate keeps them — the
                                // agents-as-tools pattern leaves the leader
                                // in control.)
                                if handoff_kind == Some(HandoffKind::Transfer) {
                                    subtasks.clear();
                                }

                                if let Some(spec) = handoff_subtask {
                                    subtasks.push(spec);
                                }

                                if !subtasks.is_empty() {
                                    let sub_events = Box::pin(rt_self.clone().apply(
                                            &room_id_owned,
                                            SchedulerEvent::PlanSubmitted {
                                                room_id: room_id_owned.clone(),
                                                parent_node_id: node_id_owned.clone(),
                                                subtasks,
                                            },
                                        ))
                                        .await;
                                    rt_self.publish_all(&room_id_owned, &sub_events);
                                } else {
                                    // Leader answered directly (no subtasks,
                                    // no handoff) OR failed to produce a
                                    // usable response. (H5) Persist the
                                    // leader's answer as a visible message;
                                    // only if there is truly nothing do we
                                    // surface a SoftError (M4) instead of
                                    // silently advancing and showing blank.
                                    let direct_answer: Option<String> =
                                        match &parsed_decision {
                                            Some(d)
                                                if !d.analysis.trim().is_empty() =>
                                            {
                                                Some(d.analysis.trim().to_string())
                                            }
                                            _ => None,
                                        };
                                    let answer = direct_answer.or_else(|| {
                                        if raw_output.trim().is_empty() {
                                            None
                                        } else {
                                            Some(raw_output.trim().to_string())
                                        }
                                    });
                                    match answer {
                                        Some(text) => {
                                            let events = rt_self
                                                .clone()
                                                .on_agent_output(
                                                    &room_id_owned,
                                                    &node_id_owned,
                                                    &agent_id_owned,
                                                    &agent_name_owned,
                                                    &text,
                                                    &[],
                                                )
                                                .await;
                                            rt_self.publish_all(&room_id_owned, &events);
                                        }
                                        None => {
                                            rt_self.publish(
                                                &room_id_owned,
                                                RoomEvent::SoftError {
                                                    room_id: room_id_owned.clone(),
                                                    code: "leader_no_output".into(),
                                                    message: "项目总管未能生成有效回复，请重试或调整请求。".into(),
                                                },
                                            );
                                            let events = rt_self
                                                .clone()
                                                .apply(
                                                    &room_id_owned,
                                                    SchedulerEvent::AgentOutputSubmitted {
                                                        room_id: room_id_owned.clone(),
                                                        node_id: node_id_owned.clone(),
                                                        agent_id: agent_id_owned.clone(),
                                                        success: false,
                                                        output_excerpt: String::new(),
                                                    },
                                                )
                                                .await;
                                            rt_self.publish_all(&room_id_owned, &events);
                                        }
                                    }
                                }
                            } else {
                                // Member path: regular save + advance.
                                let events = rt_self
                                    .clone()
                                    .on_agent_output(
                                        &room_id_owned,
                                        &node_id_owned,
                                        &agent_id_owned,
                                        &agent_name_owned,
                                        &raw_output,
                                        &[],
                                    )
                                    .await;
                                rt_self.publish_all(&room_id_owned, &events);
                            }
                        }
                        Err(e) => {
                            // Dispatch failed — surface as DispatchFailed
                            // so the user sees *why* nobody replied
                            // (2026-07 refactor, was silent SoftError).
                            rt_self.publish(
                                &room_id_owned,
                                RoomEvent::DispatchFailed {
                                    room_id: room_id_owned.clone(),
                                    agent_id: agent_id_owned.clone(),
                                    agent_name: agent_name_owned.clone(),
                                    reason: e,
                                    is_leader: agent_id_owned == "__leader__",
                                },
                            );
                            // Feed the failure back to the scheduler so
                            // the task node transitions out of Executing
                            // and the room doesn't get stuck. Without this,
                            // RoomCompleted is never emitted and the WS
                            // handler hangs indefinitely waiting for it.
                            let fail_events = rt_self
                                .clone()
                                .apply(
                                    &room_id_owned,
                                    SchedulerEvent::AgentOutputSubmitted {
                                        room_id: room_id_owned.clone(),
                                        node_id: node_id_owned.clone(),
                                        agent_id: agent_id_owned.clone(),
                                        success: false,
                                        output_excerpt: String::new(),
                                    },
                                )
                                .await;
                            rt_self.publish_all(&room_id_owned, &fail_events);
                        }
                    }
                } else {
                    emitted.push(RoomEvent::BusDelivered {
                        room_id: room_id.clone(),
                        agent_id: agent_id.clone(),
                        message_kind: "Dispatch".to_string(),
                        text: format!("Node {} → {} dispatched", node_id, description),
                    });
                }
            }
            SchedulerAction::RequestReview {
                node_id,
                reviewer_id,
            } => {
                emitted.push(RoomEvent::BusDelivered {
                    room_id: room_id.clone(),
                    agent_id: reviewer_id.clone(),
                    message_kind: "RequestReview".to_string(),
                    text: format!("Please review node {}", node_id),
                });
            }
            SchedulerAction::IssuePenalty {
                room_id: _,
                agent_id,
                violation_type,
                deduction,
                score_after,
                red_line,
                violation_id,
            } => {
                let level = if red_line { "RedLine" } else { "Severe" };
                emitted.push(RoomEvent::AuditVerdict {
                    room_id: room_id.clone(),
                    agent_id: agent_id.clone(),
                    violation_id: violation_id.clone(),
                    level: level.to_string(),
                    score_delta: deduction,
                    score_after,
                    reason: violation_type.clone(),
                });
                if red_line || score_after <= 0 {
                    emitted.push(RoomEvent::AgentExpelled {
                        room_id: room_id.clone(),
                        agent_id: agent_id.clone(),
                        agent_name: agent_id.clone(),
                        reason: violation_type.clone(),
                    });
                }
            }
            SchedulerAction::AgentExpelled {
                room_id: _,
                agent_id,
                reason,
            } => {
                emitted.push(RoomEvent::AgentExpelled {
                    room_id: room_id.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_id.clone(),
                    reason: reason.clone(),
                });
            }
            SchedulerAction::RoomCompleted { room_id: _ } => {
                emitted.push(RoomEvent::RoomCompleted {
                    room_id: room_id.clone(),
                });
                // P1.8 — team quality bonus. Count pass/fail reviews
                // across all task nodes. More passes than fails →
                // +10 per member. More fails → -10. Equal → 0.
                let (passes, fails, agents): (u32, u32, Vec<String>) = {
                    let rt = room.read();
                    let mut p = 0u32;
                    let mut f = 0u32;
                    let mut seen = std::collections::HashSet::new();
                    let mut ids = Vec::new();
                    let leader = super::persistence::LEADER_AGENT_ID;
                    for node in rt.scheduler_state.nodes.values() {
                        if let Some(ref review) = node.review {
                            if review.passed { p += 1; } else { f += 1; }
                        }
                        if node.assigned_agent != leader
                            && seen.insert(node.assigned_agent.clone())
                        {
                            ids.push(node.assigned_agent.clone());
                        }
                    }
                    (p, f, ids)
                };
                let delta: i32 = if passes > fails {
                    10
                } else if fails > passes {
                    -10
                } else {
                    0
                };
                if delta != 0 {
                    for agent_id in &agents {
                        let _ = self
                            .manager
                            .adjust_member_score(&room_id, agent_id, delta)
                            .await;
                    }
                }
            }
            SchedulerAction::Persist { room_id, node_id } => {
                // Flush a single node's current state (phase, output,
                // review verdict) to the subtasks table. Best-effort:
                // log on error but never block the workflow.
                let node = {
                    let rt = room.read();
                    rt.scheduler_state.nodes.get(&node_id).cloned()
                };
                if let Some(node) = node {
                    if let Err(e) =
                        self.manager.save_subtask_from_engine(&room_id, &node)
                    {
                        tracing::warn!(
                            "save_subtask_from_engine failed for node {}: {}",
                            node_id,
                            e
                        );
                    }
                }
            }
            SchedulerAction::Idle => {}
        }
        emitted
    }

    /// Process an inbound agent reply end-to-end: strip meta, run audit,
    /// feed result back to scheduler, emit events. Returns events to send
    /// to the client.
    pub async fn on_agent_output(
        self: Arc<Self>,
        room_id: &str,
        node_id: &str,
        agent_id: &str,
        agent_name: &str,
        raw_output: &str,
        tool_calls: &[ToolCallRecord],
    ) -> Vec<RoomEvent> {
        let mut emitted = Vec::new();

        // 1. Parse + strip META block from raw output.
        let (clean_output, meta) = parse_meta(raw_output);

        // 2. Persist agent message (with metadata if present).
        let meta_str = serde_json::to_string(&meta).ok();
        if let Err(e) = self
            .manager
            .save_agent_message_with_meta(
                room_id,
                agent_id,
                agent_name,
                &clean_output,
                MessageType::Chat,
                meta_str,
            )
            .await
        {
            emitted.push(RoomEvent::SoftError {
                room_id: room_id.to_string(),
                code: "persist_agent_message".into(),
                message: e,
            });
        }
        emitted.push(RoomEvent::MessageSaved {
            room_id: room_id.to_string(),
            message_id: uuid::Uuid::new_v4().to_string(),
            sender_id: agent_id.to_string(),
            sender_name: agent_name.to_string(),
            content: clean_output.clone(),
            is_agent: true,
        });

        // P3 — store META summary (Layer A ~50 chars) and key points
        // (Layer B ~200 chars) into the task node so the next call's
        // context assembler can inject them instead of the full output.
        {
            let rt = self.room(room_id).await;
            let node_has_meta = {
                let guard = rt.read();
                guard.scheduler_state.nodes.get(node_id)
                    .map(|_node| !meta.summary.trim().is_empty() || !meta.key_points.is_empty())
                    .unwrap_or(false)
            };
            if node_has_meta {
                let mut guard = rt.write();
                if let Some(node) = guard.scheduler_state.nodes.get_mut(node_id) {
                    if !meta.summary.is_empty() {
                        node.summary_a = Some(meta.summary.trim().to_string());
                    }
                    if !meta.key_points.is_empty() {
                        node.summary_b = Some(meta.key_points.join(" | "));
                    }
                }
            }
        }

        // 3. Run Yanluo audit.
        let agent_score_before: i32 = self
            .manager
            .get_agent_score(room_id, agent_id)
            .await
            .unwrap_or(Some(100))
            .unwrap_or(100);
        let audit_ctx = AuditContext {
            room_id: room_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            required_length: None,
            agent_meta_progress: meta.progress.map(|p| p as u8),
            output: clean_output.clone(),
            tool_calls: tool_calls.to_vec(),
            current_topic: node_id.to_string(),
            declared_session_id: Some(room_id.to_string()),
            other_room_sessions: vec![],
        };
        let _ = self.room(room_id).await;
        let violations: Vec<ViolationRecord> = {
            let mut audit = self.audit.lock();
            audit.evaluate(&audit_ctx, agent_score_before)
        };

        // 4. Translate each violation -> scheduler AuditVerdict event.
        for rec in violations {
            // Persist score change.
            let _ = self
                .manager
                .set_agent_score(room_id, agent_id, rec.score_after)
                .await;
            let event = SchedulerEvent::AuditVerdict {
                room_id: room_id.to_string(),
                agent_id: agent_id.to_string(),
                violation_type: rec.violation_type.clone(),
                severity: format!("{:?}", rec.severity).to_lowercase(),
                deduction: rec.deduction,
                score_before: rec.score_before,
                score_after: rec.score_after,
                red_line: matches!(rec.severity, ViolationLevel::RedLine),
                violation_id: rec.id.clone(),
            };
            let downstream = Box::pin(self.clone().apply(room_id, event)).await;
            emitted.extend(downstream);
        }

        // 5. Feed agent output success back to scheduler.
        let event = SchedulerEvent::AgentOutputSubmitted {
            room_id: room_id.to_string(),
            node_id: node_id.to_string(),
            agent_id: agent_id.to_string(),
            success: true,
            output_excerpt: clean_output,
        };
        let downstream = self.clone().apply(room_id, event).await;
        emitted.extend(downstream);
        self.publish_all(room_id, &emitted);
        emitted
    }

    /// User typed a message. Saves + broadcasts + dispatches.
    ///
    /// In `task_collab` mode (default): dispatches to the leader, who
    /// decides whether to answer directly, delegate subtasks, or initiate
    /// a discussion.
    ///
    /// In `free_chat` mode: dispatches to ALL members in parallel — each
    /// member sees the user's message + room context and responds
    /// independently. No leader coordination, no subtask delegation.
    pub async fn on_user_message(
        self: Arc<Self>,
        room_id: &str,
        user_id: &str,
        user_name: &str,
        content: &str,
    ) -> Vec<RoomEvent> {
        // P2.1: Cancel any active discussion when a new user message arrives.
        let _ = self.cancel_active_discussion(room_id).await;

        // Check chat_mode to determine dispatch strategy.
        let chat_mode = self
            .manager
            .get_room(room_id)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.chat_mode)
            .unwrap_or_else(|| "task_collab".to_string());

        if chat_mode == "free_chat" {
            return self
                .on_user_message_free_chat(room_id, user_id, user_name, content)
                .await;
        }

        // ── task_collab mode (default) ──
        let mut emitted = Vec::new();
        let msg = match self
            .manager
            .send_message(room_id, user_id, user_name, content)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                emitted.push(RoomEvent::SoftError {
                    room_id: room_id.to_string(),
                    code: "persist_user_message".into(),
                    message: e,
                });
                // Publish the error + RoomCompleted so the WS handler
                // doesn't hang waiting for a response that never comes.
                self.publish_all(room_id, &emitted);
                self.publish(
                    room_id,
                    RoomEvent::RoomCompleted {
                        room_id: room_id.to_string(),
                    },
                );
                emitted.push(RoomEvent::RoomCompleted {
                    room_id: room_id.to_string(),
                });
                return emitted;
            }
        };
        emitted.push(RoomEvent::MessageSaved {
            room_id: msg.room_id.clone(),
            message_id: msg.id.clone(),
            sender_id: msg.sender_id.clone(),
            sender_name: msg.sender_name.clone(),
            content: msg.content.clone(),
            is_agent: false,
        });

        // Bootstrap: create a top-level task node for the user request and
        // dispatch to the leader.
        let node_id = format!("user-{}", uuid::Uuid::new_v4());
        let event = SchedulerEvent::TaskCreated {
            room_id: room_id.to_string(),
            node_id: node_id.clone(),
            description: content.chars().take(80).collect(),
            spec: content.to_string(),
            assigned_agent: "__leader__".to_string(),
        };
        let downstream = self.clone().apply(room_id, event).await;
        emitted.extend(downstream);
        self.publish_all(room_id, &emitted);
        emitted
    }

    /// Free-chat mode: all members respond independently in parallel.
    ///
    /// No leader coordination, no subtask delegation, no audit. Each
    /// member sees the user's message + recent room context and replies
    /// freely. This is the "闲聊" mode where the chat room behaves like
    /// a real group chat — everyone can talk at once.
    async fn on_user_message_free_chat(
        self: Arc<Self>,
        room_id: &str,
        user_id: &str,
        user_name: &str,
        content: &str,
    ) -> Vec<RoomEvent> {
        let mut emitted = Vec::new();

        // Save user message.
        let msg = match self
            .manager
            .send_message(room_id, user_id, user_name, content)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                emitted.push(RoomEvent::SoftError {
                    room_id: room_id.to_string(),
                    code: "persist_user_message".into(),
                    message: e,
                });
                self.publish_all(room_id, &emitted);
                self.publish(
                    room_id,
                    RoomEvent::RoomCompleted {
                        room_id: room_id.to_string(),
                    },
                );
                emitted.push(RoomEvent::RoomCompleted {
                    room_id: room_id.to_string(),
                });
                return emitted;
            }
        };
        emitted.push(RoomEvent::MessageSaved {
            room_id: msg.room_id.clone(),
            message_id: msg.id.clone(),
            sender_id: msg.sender_id.clone(),
            sender_name: msg.sender_name.clone(),
            content: msg.content.clone(),
            is_agent: false,
        });

        // Publish the user message immediately so the frontend sees it.
        self.publish_all(room_id, &emitted);

        // Get all members (excluding the built-in leader).
        let agents = self
            .manager
            .get_room_agents(room_id)
            .await
            .unwrap_or_default();
        let members: Vec<AgentRoomProfile> = agents
            .into_iter()
            .filter(|a| a.agent_id != super::persistence::LEADER_AGENT_ID && a.role == AgentRole::Member)
            .collect();

        if members.is_empty() {
            // No members in the room — emit RoomCompleted so the frontend
            // doesn't hang waiting for a response.
            self.publish(
                room_id,
                RoomEvent::RoomCompleted {
                    room_id: room_id.to_string(),
                },
            );
            emitted.push(RoomEvent::RoomCompleted {
                room_id: room_id.to_string(),
            });
            return emitted;
        }

        // Dispatch to all members in parallel.
        let mut handles = Vec::new();
        for member in members {
            let rt = self.clone();
            let room_id_owned = room_id.to_string();
            let content_owned = content.to_string();
            handles.push(tokio::spawn(async move {
                rt.dispatch_free_chat_member(&room_id_owned, &member, &content_owned)
                    .await;
            }));
        }

        // Wait for all members to finish.
        for handle in handles {
            let _ = handle.await;
        }

        // Signal completion so the WS handler can close gracefully.
        self.publish(
            room_id,
            RoomEvent::RoomCompleted {
                room_id: room_id.to_string(),
            },
        );
        emitted.push(RoomEvent::RoomCompleted {
            room_id: room_id.to_string(),
        });
        emitted
    }

    /// Dispatch a single member's response in free-chat mode.
    ///
    /// Streams the member's LLM response via `AgentThinkingChunk` events,
    /// saves the final response, and emits `AgentThinkingCompleted` +
    /// `MessageSaved`. Errors are surfaced as `DispatchFailed`.
    async fn dispatch_free_chat_member(
        self: Arc<Self>,
        room_id: &str,
        member: &AgentRoomProfile,
        content: &str,
    ) {
        let dispatcher = match self.manager.dispatcher() {
            Some(d) => d,
            None => {
                self.publish(
                    room_id,
                    RoomEvent::SoftError {
                        room_id: room_id.to_string(),
                        code: "no_dispatcher".into(),
                        message: "没有可用的 agent dispatcher".into(),
                    },
                );
                return;
            }
        };

        let agent_name = member
            .display_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| member.agent_id.clone());
        let session_id = member.session_id.clone();

        // Build room context from recent messages.
        let messages = self
            .manager
            .get_messages(room_id, 20)
            .await
            .unwrap_or_default();
        let room_context = GroupChatManager::build_room_context(&messages, 10);

        let system_prompt = format!(
            "你是{}，在群聊室中参与自由聊天。\
             你有自己的个性和专长，请自然地回复用户的消息。\
             保持简洁、有个性、有观点。不要复述别人的话。\n\
             房间上下文：\n{}",
            agent_name, room_context
        );

        // Streaming chunk callback.
        let rt_for_chunks = self.clone();
        let room_id_owned = room_id.to_string();
        let agent_id_owned = member.agent_id.clone();
        let agent_name_owned = agent_name.clone();
        let first_emitted = Arc::new(AtomicBool::new(false));
        let first_emitted_ref = first_emitted.clone();

        let on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync> =
            Box::new(move |delta: StreamDelta| {
                let is_first = !first_emitted_ref.swap(true, Ordering::SeqCst);
                let (name, role) = if is_first {
                    (agent_name_owned.clone(), "member".to_string())
                } else {
                    (String::new(), String::new())
                };
                rt_for_chunks.publish(
                    &room_id_owned,
                    RoomEvent::AgentThinkingChunk {
                        room_id: room_id_owned.clone(),
                        agent_id: agent_id_owned.clone(),
                        agent_name: name,
                        role,
                        delta: delta.content,
                    },
                );
            });

        let result = dispatcher
            .dispatch_subtask_stream(
                room_id,
                &member.agent_id,
                &session_id,
                &system_prompt,
                content,
                "member",
                on_chunk,
            )
            .await;

        // Always emit AgentThinkingCompleted so the frontend clears
        // the typing indicator, even on errors.
        self.publish(
            room_id,
            RoomEvent::AgentThinkingCompleted {
                room_id: room_id.to_string(),
                agent_id: member.agent_id.clone(),
            },
        );

        match result {
            Ok(text) => {
                let clean = text.trim();
                if !clean.is_empty() {
                    // Persist agent message.
                    let _ = self
                        .manager
                        .save_agent_message(
                            room_id,
                            &member.agent_id,
                            &agent_name,
                            clean,
                            MessageType::Chat,
                        )
                        .await;
                }
            }
            Err(e) => {
                self.publish(
                    room_id,
                    RoomEvent::DispatchFailed {
                        room_id: room_id.to_string(),
                        agent_id: member.agent_id.clone(),
                        agent_name: agent_name.clone(),
                        reason: e,
                        is_leader: false,
                    },
                );
            }
        }
    }

    /// Inbound review verdict from leader / human reviewer.
    /// /// P1 reward hook: passing/failing a review moves the *assigned*
    /// agent's rank by ±5 (设计 §一). The delta is applied after the
    /// scheduler runs so we can read the node's `assigned_agent` from
    /// the room state without re-querying the DB. The score write is
    /// best-effort — a rank failure does NOT block the verdict from
    /// propagating (same policy as the update_subtask P3.3 hook).
    /// The `__leader__` sentinel is skipped (it never owns a real
    /// subtask so its row would be a no-op anyway).
    pub async fn on_review_verdict(
        self: Arc<Self>,
        room_id: &str,
        node_id: &str,
        passed: bool,
        comment: &str,
        reviewer_id: &str,
    ) -> Vec<RoomEvent> {
        let event = SchedulerEvent::ReviewVerdict {
            room_id: room_id.to_string(),
            node_id: node_id.to_string(),
            result: ReviewResult {
                passed,
                comment: comment.to_string(),
                reviewer_id: reviewer_id.to_string(),
                reviewed_at: Utc::now(),
            },
        };
        let emitted = self.clone().apply(room_id, event).await;
        // post-apply state (the scheduler may have re-dispatched on
        // fail, but the assigned_agent field is stable).
        let assigned_agent = {
            let room = self.room(room_id).await;
            let rt = room.read();
            rt.scheduler_state
                .nodes
                .get(node_id)
                .map(|n| n.assigned_agent.clone())
                .unwrap_or_default()
        };
        if !assigned_agent.is_empty()
            && assigned_agent != super::persistence::LEADER_AGENT_ID
        {
            let delta: i32 = if passed { 5 } else { -5 };
            if let Err(e) = self
                .manager
                .adjust_member_score(room_id, &assigned_agent, delta)
                .await
            {
                tracing::warn!(
                    "on_review_verdict rank hook failed for room={} agent={} delta={}: {}",
                    room_id, assigned_agent, delta, e
                );
            }
            // P1.2 — bump the consecutive-pass streak. Pass increments
            // and updates max_streak; fail resets to 0. Milestone bonus
            // (+10) fires when the new streak is a positive even number
            // (2, 4, 6, ...). Same best-effort / non-blocking contract
            // as the ±5 score hook above.
            match self
                .manager
                .bump_streak(room_id, &assigned_agent, passed)
                .await
            {
                Ok(new_streak) if passed && new_streak > 0 && new_streak % 2 == 0 => {
                    if let Err(e) = self
                        .manager
                        .adjust_member_score(room_id, &assigned_agent, 10)
                        .await
                    {
                        tracing::warn!(
                            "on_review_verdict streak bonus failed for room={} agent={} streak={}: {}",
                            room_id, assigned_agent, new_streak, e
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        "on_review_verdict streak hook failed for room={} agent={} passed={}: {}",
                        room_id, assigned_agent, passed, e
                    );
                }
            }
        }
        emitted
    }

    /// P4 — agent disputes a violation. Marks it Disputed in the
    /// audit trail so the leader sees it. Returns the violation's
    /// deduction amount on success, or `None` if not found.
    pub fn dispute_violation(&self, violation_id: &str, reason: &str) -> Option<i32> {
        let mut audit = self.audit.lock();
        audit.dispute(violation_id, reason)
    }

    /// P4 — leader overturns a disputed violation. Restores the
    /// deducted score and marks the record as Overturned.
    /// Returns `Ok(deduction)` on success, `Err(())` if the
    /// violation wasn't found or wasn't in a disputable state.
    pub async fn overturn_violation(
        &self,
        room_id: &str,
        violation_id: &str,
        agent_id: &str,
        reviewer: &str,
        comment: &str,
    ) -> Result<i32, ()> {
        let deduction = {
            let mut audit = self.audit.lock();
            if !audit.overturn(violation_id, reviewer, comment) {
                return Err(());
            }
            // Re-read the record to get the deduction amount.
            audit.trail.all_records()
                .iter()
                .find(|r| r.id == violation_id)
                .map(|r| r.deduction)
                .unwrap_or(0)
        };
        // Restore the deducted score.
        let _ = self.manager
            .adjust_member_score(room_id, agent_id, deduction)
            .await;
        Ok(deduction)
    }

    /// Inbound spec update mid-flight.
    pub async fn on_spec_updated(
        self: Arc<Self>,
        room_id: &str,
        node_id: &str,
        new_spec: &str,
    ) -> Vec<RoomEvent> {
        let event = SchedulerEvent::SpecUpdated {
            room_id: room_id.to_string(),
            node_id: node_id.to_string(),
            new_spec: new_spec.to_string(),
        };
        self.clone().apply(room_id, event).await
    }

    /// Inbound cancel.
    pub async fn on_cancelled(
        self: Arc<Self>,
        room_id: &str,
        node_id: Option<&str>,
    ) -> Vec<RoomEvent> {
        let event = SchedulerEvent::Cancelled {
            room_id: room_id.to_string(),
            node_id: node_id.map(|s| s.to_string()),
        };
        self.clone().apply(room_id, event).await
    }

    /// Inbound plan submission (leader outputs subtasks).
    pub async fn on_plan_submitted(
        self: Arc<Self>,
        room_id: &str,
        parent_node_id: &str,
        subtasks: Vec<SubtaskSpec>,
    ) -> Vec<RoomEvent> {
        let event = SchedulerEvent::PlanSubmitted {
            room_id: room_id.to_string(),
            parent_node_id: parent_node_id.to_string(),
            subtasks,
        };
        self.clone().apply(room_id, event).await
    }

    /// Spawn a discussion in the background (P1.6 + P2.1).
    ///
    /// Called from `dispatch_action` when the leader's decision includes
    /// a `DiscussionConfig`. The discussion runs on a `tokio::spawn` task
    /// so it doesn't block the current dispatch. The `CancellationToken`
    /// is stored on `RoomRuntime.active_discussion` so a subsequent user
    /// message can cancel it.
    pub async fn spawn_discussion(
        self: Arc<Self>,
        room_id: &str,
        node_id: &str,
        config: DiscussionConfig,
        trigger_message: &str,
    ) {
        let cancel_token = CancellationToken::new();
        let discussion = ActiveDiscussion {
            discussion_id: uuid::Uuid::new_v4().to_string(),
            cancel_token: cancel_token.clone(),
        };

        // Set the node to Discussing phase.
        {
            let room = self.room(room_id).await;
            let mut rt = room.write();
            rt.active_discussion = Some(discussion);
            if let Some(node) = rt.scheduler_state.nodes.get_mut(node_id) {
                node.phase = TaskPhase::Discussing;
            }
        }

        let engine = Arc::new(DiscussionEngine::new(
            self.manager.clone(),
            self.clone(),
        ));

        let room_id_owned = room_id.to_string();
        let node_id_owned = node_id.to_string();
        let trigger_owned = trigger_message.to_string();
        let rt_self = self.clone();

        tokio::spawn(async move {
            let result = engine
                .run(
                    &room_id_owned,
                    &node_id_owned,
                    config,
                    &trigger_owned,
                    cancel_token,
                )
                .await;

            // Clear active_discussion.
            {
                let room = rt_self.room(&room_id_owned).await;
                let mut rt = room.write();
                rt.active_discussion = None;
                // Mark node as completed.
                if let Some(node) = rt.scheduler_state.nodes.get_mut(&node_id_owned) {
                    node.phase = TaskPhase::Completed;
                }
            }

            match result {
                Ok(state) => {
                    tracing::info!(
                        room_id = %room_id_owned,
                        discussion_id = %state.discussion_id,
                        rounds = state.current_round,
                        "discussion completed"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        room_id = %room_id_owned,
                        error = %e,
                        "discussion failed"
                    );
                    rt_self.publish(
                        &room_id_owned,
                        RoomEvent::SoftError {
                            room_id: room_id_owned.clone(),
                            code: "discussion_failed".into(),
                            message: format!("讨论失败: {}", e),
                        },
                    );
                }
            }
        });
    }

    /// Cancel any active discussion in a room (P2.1).
    ///
    /// Called from `on_user_message` when a new user message arrives
    /// during an active discussion. The cancellation token causes the
    /// discussion task to exit at the next safe point.
    pub async fn cancel_active_discussion(&self, room_id: &str) -> bool {
        let room = self.room(room_id).await;
        let mut rt = room.write();
        if let Some(discussion) = rt.active_discussion.take() {
            discussion.cancel_token.cancel();
            tracing::info!(
                room_id = %room_id,
                discussion_id = %discussion.discussion_id,
                "discussion cancelled by user message"
            );
            true
        } else {
            false
        }
    }
}

/// Implementation of `DiscussionEventPublisher` for `TaskEngineRuntime`.
///
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
fn phase_label(phase: &TaskPhase) -> &'static str {
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
    /// /// adjust_member_score upserts at `default_for()` (score 10) on
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
