//! Action dispatch: apply() and dispatch_action() state-machine handling.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::groupchat::agent_contract::{HandoffDirective, LeaderDecision};
use crate::groupchat::dispatcher::StreamDelta;
use crate::groupchat::handoff::{resolve_handoff, HandoffKind};
use crate::groupchat::leader::LeaderOrchestrator;
use crate::groupchat::scheduler::{SchedulerAction, SchedulerEvent, SubtaskSpec};

use super::*;

impl TaskEngineRuntime {
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
    pub(crate) async fn dispatch_action(
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

                                //`needs_user_confirmation` is parsed but
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
                                        //(: Delegate now also synthesizes
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
                                    //surface a SoftError instead of
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
                    let leader = crate::groupchat::persistence::LEADER_AGENT_ID;
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
}
