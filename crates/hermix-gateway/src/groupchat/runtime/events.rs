//! Inbound event handling: agent output, user messages, review, disputes, discussions.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::groupchat::dispatcher::StreamDelta;
use crate::groupchat::discussion::{ActiveDiscussion, DiscussionConfig, DiscussionEngine};
use crate::groupchat::manager::GroupChatManager;
use crate::groupchat::meta::parse_meta;
use crate::groupchat::persistence::{AgentRole, AgentRoomProfile, MessageType};
use crate::groupchat::scheduler::{SchedulerEvent, SubtaskSpec};
use crate::groupchat::task_types::{ReviewResult, TaskPhase, ViolationLevel, ViolationRecord};
use crate::groupchat::yanluo::{AuditContext, ToolCallRecord};

use super::*;

impl TaskEngineRuntime {
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
    pub(crate) async fn on_user_message_free_chat(
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
            .filter(|a| a.agent_id != crate::groupchat::persistence::LEADER_AGENT_ID && a.role == AgentRole::Member)
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
    pub(crate) async fn dispatch_free_chat_member(
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
    /// P1 reward hook: passing/failing a review moves the *assigned*
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
            && assigned_agent != crate::groupchat::persistence::LEADER_AGENT_ID
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
