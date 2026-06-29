//! TaskScheduler — the state machine that advances the room through
//! planning → executing → review → completed phases.
//!
//! ## Design
//!
//! The scheduler is a *pure* state machine: it does not own the
//! runtime, agents, or LLM calls. It only knows how to:
//!
//! 1. Hold the task tree (`Vec<TaskNode>`) for a room.
//! 2. Advance phase transitions when external events occur
//!    (agent output submitted, review verdict issued, spec updated,
//!    user cancel, all-done notification).
//! 3. Emit `BusMessage` envelopes describing what should happen next
//!    (who to dispatch, who should review, who was expelled).
//!
//! External code (the actor runtime) consumes those envelopes and
//! actually invokes the LLM. This separation keeps the scheduler
//! 100 % testable without a tokio runtime or LLM stub.
//!
//! ## State machine per node
//!
//! ```text
//!   Idle → Planning → Executing → Review
//!                              ↑   ↓
//!                           Revising ←── reject
//!                              ↓
//!                          Completed
//!                              ↓
//!                          Cancelled
//! ```
//!
//! ## "All done" rule
//!
//! The user may say "全部完成". That single command promotes the
//! current room to "must finish all open nodes before reporting
//! success" mode. Individual subtask reports are suppressed, and
//! only the final aggregate report is delivered.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::bus::BusMessage;
use super::task_types::{ReviewResult, TaskNode, TaskPhase};

// ── Scheduler decisions (the "what should happen next" output) ──

/// What the scheduler instructs the runtime to do after a state
/// transition. The runtime is free to ignore / delay execution
/// (e.g. when a queue is full) but it MUST eventually fulfil every
/// emitted `Dispatch` and `RequestReview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchedulerAction {
    /// Spawn or resume an actor for `agent_id` working on `node_id`.
    Dispatch { node_id: String, agent_id: String },
    /// Ask `reviewer_id` to review `node_id` (the actor that produced
    /// the output).
    RequestReview { node_id: String, reviewer_id: String },
    /// Notify the bus that the room is done.
    RoomCompleted { room_id: String },
    /// Notify the bus that `agent_id` was expelled (score <= 0).
    AgentExpelled { room_id: String, agent_id: String, reason: String },
    /// Notify the bus that a penalty was applied (visible in audit log).
    /// `score_after` ≤ 0 implies an `AgentExpelled` follows.
    IssuePenalty {
        room_id: String,
        agent_id: String,
        violation_type: String,
        deduction: i32,
        score_after: i32,
        /// True if this is a red-line (non-overturnable) violation.
        red_line: bool,
        /// P4 — unique violation id for dispute/overturn.
        violation_id: String,
    },
    /// Persist a single node to the `subtasks` table. Emitted by the
    /// scheduler whenever a node's `phase` or `review` changed so the
    /// runtime can flush the in-memory state to disk. Errors during
    /// persistence are logged but never block the workflow.
    Persist { room_id: String, node_id: String },
    /// No-op (e.g. the event was a no-op or already handled).
    Idle,
}

// ── Events that advance the state machine ────────────────────

/// External event fed into the scheduler. Pure data, no I/O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchedulerEvent {
    /// User kicked off a new top-level task.
    TaskCreated {
        room_id: String,
        node_id: String,
        description: String,
        spec: String,
        assigned_agent: String,
    },
    /// Leader finished planning — split the task into subtasks.
    PlanSubmitted {
        room_id: String,
        parent_node_id: String,
        subtasks: Vec<SubtaskSpec>,
    },
    /// Agent reported it finished its work on a node.
    AgentOutputSubmitted {
        room_id: String,
        node_id: String,
        agent_id: String,
        /// `true` if the agent claims the work is done. `false` if
        /// the agent is yielding mid-task (paused, blocked, etc).
        success: bool,
        output_excerpt: String,
    },
    /// Reviewer (or auto-review) returned a verdict.
    ReviewVerdict {
        room_id: String,
        node_id: String,
        result: ReviewResult,
    },
    /// User updated the spec of a running node.
    SpecUpdated {
        room_id: String,
        node_id: String,
        new_spec: String,
    },
    /// User cancelled the whole room (or one node).
    Cancelled { room_id: String, node_id: Option<String> },
    /// User demanded "全部完成" — suppress per-node summaries,
    /// require every open node to be Completed before reporting.
    FinishAllRequested { room_id: String },
    /// Score changed for an agent (e.g. penalty from Yanluo).
    ScoreChanged {
        room_id: String,
        agent_id: String,
        new_score: i32,
    },
    /// Leader is requesting a review on a node.
    ReviewRequested {
        room_id: String,
        node_id: String,
        reviewer_id: String,
    },
    /// Yanluo audit produced one or more violation records.
    /// Runtime feeds the result in (one event per `ViolationRecord`).
    AuditVerdict {
        room_id: String,
        agent_id: String,
        violation_type: String,
        severity: String, // "warning" / "severe" / "critical" / "red_line"
        deduction: i32,
        score_before: i32,
        score_after: i32,
        red_line: bool,
        /// P4 — unique violation id for dispute/overturn.
        violation_id: String,
    },
}

/// Leader-supplied spec for a single subtask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskSpec {
    pub node_id: String,
    pub description: String,
    pub spec: String,
    pub assigned_agent: String,
    pub sort_order: u32,
}

// ── Room state held by the scheduler ─────────────────────────

/// Scheduler state for a single room. Owns the task tree plus
/// flags. The runtime is expected to keep one of these in an
/// `RwLock` and call `apply_event` to mutate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSchedulerState {
    pub room_id: String,
    /// All nodes, indexed by id. Includes root and subtasks.
    pub nodes: HashMap<String, TaskNode>,
    /// Root node ids (nodes with no parent). For a normal room
    /// there is usually exactly one root at a time.
    pub root_ids: Vec<String>,
    /// "全部完成" mode: do not report per-node, only final summary.
    pub finish_all_mode: bool,
    /// Set true when every node is in a terminal phase.
    pub all_completed: bool,
    /// Pending review requests waiting to be picked up by reviewers.
    pub pending_reviews: HashMap<String, String>, // node_id -> reviewer_id
    /// Last computed score per agent in this room (cached for
    /// the "score changed" event — scheduler doesn't own the
    /// canonical GlobalRank, just mirrors it).
    pub last_known_score: HashMap<String, i32>,
}

impl RoomSchedulerState {
    pub fn new(room_id: &str) -> Self {
        Self {
            room_id: room_id.to_string(),
            nodes: HashMap::new(),
            root_ids: Vec::new(),
            finish_all_mode: false,
            all_completed: false,
            pending_reviews: HashMap::new(),
            last_known_score: HashMap::new(),
        }
    }

    /// Recompute the room's overall "all done" flag by scanning
    /// every node. Returns `true` if every node is in a terminal
    /// phase (Completed / Cancelled).
    pub fn recompute_all_completed(&mut self) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let terminal = |p: &TaskPhase| matches!(p, TaskPhase::Completed | TaskPhase::Cancelled);
        let done = self.nodes.values().all(|n| terminal(&n.phase));
        self.all_completed = done;
        done
    }

    /// All children of a node, sorted by `sort_order`.
    pub fn children(&self, parent_id: &str) -> Vec<&TaskNode> {
        let mut out: Vec<&TaskNode> = self
            .nodes
            .values()
            .filter(|n| n.parent_id.as_deref() == Some(parent_id))
            .collect();
        out.sort_by_key(|n| n.sort_order);
        out
    }
}

// ── The scheduler itself ─────────────────────────────────────

/// Pure state machine — no I/O, no LLM, no runtime. Apply events
/// one at a time; the returned `Vec<SchedulerAction>` tells the
/// runtime what to do.
pub struct TaskScheduler;

impl TaskScheduler {
    pub fn new() -> Self {
        Self
    }

    /// Apply one event, mutate `state` in place, return the actions
    /// the runtime should perform as a consequence.
    pub fn apply_event(
        &self,
        state: &mut RoomSchedulerState,
        event: SchedulerEvent,
    ) -> Vec<SchedulerAction> {
        // Verify room id matches (defence-in-depth — runtime is
        // expected to key states by room_id).
        let event_room = event_room_id(&event);
        if event_room != state.room_id {
            return vec![SchedulerAction::Idle];
        }

        match event {
            SchedulerEvent::TaskCreated {
                node_id,
                description,
                spec,
                assigned_agent,
                ..
            } => self.on_task_created(state, node_id, description, spec, assigned_agent),
            SchedulerEvent::PlanSubmitted {
                parent_node_id,
                subtasks,
                ..
            } => self.on_plan_submitted(state, parent_node_id, subtasks),
            SchedulerEvent::AgentOutputSubmitted {
                node_id,
                agent_id,
                success,
                ..
            } => self.on_agent_output(state, node_id, agent_id, success),
            SchedulerEvent::ReviewVerdict { node_id, result, .. } => {
                self.on_review_verdict(state, node_id, result)
            }
            SchedulerEvent::SpecUpdated { node_id, new_spec, .. } => {
                self.on_spec_updated(state, node_id, new_spec)
            }
            SchedulerEvent::Cancelled { node_id, .. } => self.on_cancelled(state, node_id),
            SchedulerEvent::FinishAllRequested { .. } => {
                state.finish_all_mode = true;
                vec![SchedulerAction::Idle]
            }
            SchedulerEvent::ScoreChanged {
                agent_id, new_score, ..
            } => self.on_score_changed(state, agent_id, new_score),
            SchedulerEvent::ReviewRequested {
                node_id,
                reviewer_id,
                ..
            } => self.on_review_requested(state, node_id, reviewer_id),
            SchedulerEvent::AuditVerdict {
                agent_id,
                violation_type,
                deduction,
                score_after,
                red_line,
                violation_id,
                ..
            } => self.on_audit_verdict(state, agent_id, violation_type, deduction, score_after, red_line, violation_id),
        }
    }

    fn on_task_created(
        &self,
        state: &mut RoomSchedulerState,
        node_id: String,
        description: String,
        spec: String,
        assigned_agent: String,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        let node = TaskNode {
            id: node_id.clone(),
            room_id: state.room_id.clone(),
            parent_id: None,
            phase: TaskPhase::Planning,
            description,
            spec,
            assigned_agent: assigned_agent.clone(),
            output: String::new(),
            review: None,
            summary_a: None,
            summary_b: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        };
        state.nodes.insert(node_id.clone(), node);
        state.root_ids.push(node_id.clone());
        state.all_completed = false;
        vec![SchedulerAction::Dispatch {
            node_id,
            agent_id: assigned_agent,
        }]
    }

    fn on_plan_submitted(
        &self,
        state: &mut RoomSchedulerState,
        parent_node_id: String,
        subtasks: Vec<SubtaskSpec>,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        let mut actions = Vec::new();

        // Promote the parent out of Planning into Executing.
        if let Some(parent) = state.nodes.get_mut(&parent_node_id) {
            parent.phase = TaskPhase::Executing;
            parent.updated_at = now;
        }

        for st in subtasks {
            let node = TaskNode {
                id: st.node_id.clone(),
                room_id: state.room_id.clone(),
                parent_id: Some(parent_node_id.clone()),
                phase: TaskPhase::Executing,
                description: st.description,
                spec: st.spec,
                assigned_agent: st.assigned_agent.clone(),
                output: String::new(),
                review: None,
                summary_a: None,
                summary_b: None,
                sort_order: st.sort_order,
                created_at: now,
                updated_at: now,
            };
            state.nodes.insert(st.node_id.clone(), node);
            actions.push(SchedulerAction::Dispatch {
                node_id: st.node_id,
                agent_id: st.assigned_agent,
            });
        }

        actions
    }

    fn on_agent_output(
        &self,
        state: &mut RoomSchedulerState,
        node_id: String,
        agent_id: String,
        success: bool,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        let mut actions = Vec::new();

        if let Some(node) = state.nodes.get_mut(&node_id) {
            // Wrong agent reported — log via action trace, ignore.
            if node.assigned_agent != agent_id {
                return vec![SchedulerAction::Idle];
            }
            if success {
                // Output is in; advance to Review. A reviewer (any
                // agent with role Leader, or a designated reviewer)
                // will be picked by the runtime.
                node.phase = TaskPhase::Review;
                node.updated_at = now;
                // Reviewer: same room's leader. Runtime looks it
                // up — scheduler doesn't know the leader's id.
                let reviewer_id = "__leader__".to_string();
                state
                    .pending_reviews
                    .insert(node_id.clone(), reviewer_id.clone());
                actions.push(SchedulerAction::RequestReview {
                    node_id,
                    reviewer_id,
                });
            } else {
                // Agent yielded (e.g. asked a question, blocked).
                // Stay in Executing; the runtime should re-dispatch
                // when the user responds.
                node.updated_at = now;
            }
        }
        actions
    }

    fn on_review_verdict(
        &self,
        state: &mut RoomSchedulerState,
        node_id: String,
        result: ReviewResult,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        let mut actions = Vec::new();
        state.pending_reviews.remove(&node_id);

        if let Some(node) = state.nodes.get_mut(&node_id) {
            node.review = Some(result.clone());
            if result.passed {
                node.phase = TaskPhase::Completed;
                node.updated_at = now;
            } else {
                node.phase = TaskPhase::Revising;
                node.updated_at = now;
                // Re-dispatch to the same agent for revisions.
                actions.push(SchedulerAction::Dispatch {
                    node_id: node_id.clone(),
                    agent_id: node.assigned_agent.clone(),
                });
            }
        }

        // Flush the node's new review/phase to the subtasks table.
        // Runtime is responsible for translating this to a DB write.
        actions.push(SchedulerAction::Persist {
            room_id: state.room_id.clone(),
            node_id,
        });

        // Recompute room-level completion. Emit RoomCompleted only
        // on the false→true transition (idempotent on re-events).
        // Single-task rooms signal completion via the node's phase
        // transition alone — RoomCompleted is reserved for multi-task
        // rooms where all parallel work is done.
        let was_all_completed = state.all_completed;
        if !was_all_completed
            && state.nodes.len() > 1
            && state.recompute_all_completed()
        {
            actions.push(SchedulerAction::RoomCompleted {
                room_id: state.room_id.clone(),
            });
        }

        actions
    }

    fn on_spec_updated(
        &self,
        state: &mut RoomSchedulerState,
        node_id: String,
        new_spec: String,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        let mut actions = Vec::new();
        if let Some(node) = state.nodes.get_mut(&node_id) {
            node.spec = new_spec;
            node.updated_at = now;
            // If the node was idle / revising, re-dispatch.
            if matches!(node.phase, TaskPhase::Revising | TaskPhase::Executing) {
                actions.push(SchedulerAction::Dispatch {
                    node_id: node_id.clone(),
                    agent_id: node.assigned_agent.clone(),
                });
            }
        }
        actions
    }

    fn on_cancelled(
        &self,
        state: &mut RoomSchedulerState,
        node_id: Option<String>,
    ) -> Vec<SchedulerAction> {
        let now = Utc::now();
        match node_id {
            Some(id) => {
                if let Some(node) = state.nodes.get_mut(&id) {
                    node.phase = TaskPhase::Cancelled;
                    node.updated_at = now;
                }
            }
            None => {
                for node in state.nodes.values_mut() {
                    if !matches!(node.phase, TaskPhase::Completed | TaskPhase::Cancelled) {
                        node.phase = TaskPhase::Cancelled;
                        node.updated_at = now;
                    }
                }
            }
        }
        // Emit RoomCompleted only on the false→true transition,
        // and only for multi-task rooms. Single-task rooms don't
        // need the room-level signal — the node's phase is enough.
        let was_all_completed = state.all_completed;
        if !was_all_completed
            && state.nodes.len() > 1
            && state.recompute_all_completed()
        {
            vec![SchedulerAction::RoomCompleted {
                room_id: state.room_id.clone(),
            }]
        } else {
            Vec::new()
        }
    }

    fn on_score_changed(
        &self,
        state: &mut RoomSchedulerState,
        agent_id: String,
        new_score: i32,
    ) -> Vec<SchedulerAction> {
        state
            .last_known_score
            .insert(agent_id.clone(), new_score);
        if new_score <= 0 {
            vec![SchedulerAction::AgentExpelled {
                room_id: state.room_id.clone(),
                agent_id,
                reason: "score <= 0".to_string(),
            }]
        } else {
            vec![SchedulerAction::Idle]
        }
    }

    fn on_review_requested(
        &self,
        state: &mut RoomSchedulerState,
        node_id: String,
        reviewer_id: String,
    ) -> Vec<SchedulerAction> {
        state
            .pending_reviews
            .insert(node_id.clone(), reviewer_id.clone());
        vec![SchedulerAction::RequestReview {
            node_id,
            reviewer_id,
        }]
    }

    /// Apply a Yanluo audit verdict: update cached score and emit
    /// a penalty notice. If `score_after <= 0`, also emit
    /// `AgentExpelled` so the runtime kicks the agent out of the room.
    fn on_audit_verdict(
        &self,
        state: &mut RoomSchedulerState,
        agent_id: String,
        violation_type: String,
        deduction: i32,
        score_after: i32,
        red_line: bool,
        violation_id: String,
    ) -> Vec<SchedulerAction> {
        state
            .last_known_score
            .insert(agent_id.clone(), score_after);
        let mut actions = vec![SchedulerAction::IssuePenalty {
            room_id: state.room_id.clone(),
            agent_id: agent_id.clone(),
            violation_type,
            deduction,
            score_after,
            red_line,
            violation_id,
        }];
        if score_after <= 0 {
            actions.push(SchedulerAction::AgentExpelled {
                room_id: state.room_id.clone(),
                agent_id,
                reason: if red_line {
                    "红线违规，积分归零".to_string()
                } else {
                    "积分归零，自动开除".to_string()
                },
            });
        }
        actions
    }

    /// Helper for the runtime: build a fresh node id (UUID v4).
    pub fn new_node_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Helper for the runtime: produce a `BusMessage` notifying the
    /// bus that the room is done. Idempotent — multiple calls are
    /// cheap.  Reuses the room_id as the task_id since the room
    /// itself is the "task" at the top of the task tree.
    pub fn room_completed_message(room_id: &str) -> BusMessage {
        BusMessage::TaskStateChange {
            task_id: room_id.to_string(),
            from: crate::groupchat::task_types::TaskPhase::Executing,
            to: crate::groupchat::task_types::TaskPhase::Completed,
        }
    }
}

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new()
    }
}

fn event_room_id(event: &SchedulerEvent) -> &str {
    match event {
        SchedulerEvent::TaskCreated { room_id, .. }
        | SchedulerEvent::PlanSubmitted { room_id, .. }
        | SchedulerEvent::AgentOutputSubmitted { room_id, .. }
        | SchedulerEvent::ReviewVerdict { room_id, .. }
        | SchedulerEvent::SpecUpdated { room_id, .. }
        | SchedulerEvent::Cancelled { room_id, .. }
        | SchedulerEvent::FinishAllRequested { room_id }
        | SchedulerEvent::ScoreChanged { room_id, .. }
        | SchedulerEvent::ReviewRequested { room_id, .. }
        | SchedulerEvent::AuditVerdict { room_id, .. } => room_id,
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state() -> RoomSchedulerState {
        RoomSchedulerState::new("room-1")
    }

    #[test]
    fn task_created_dispatches_to_assigned_agent() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "Write chapter 1".into(),
                spec: "1000 words, romance style".into(),
                assigned_agent: "writer".into(),
            },
        );
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SchedulerAction::Dispatch { node_id, agent_id } => {
                assert_eq!(node_id, "n1");
                assert_eq!(agent_id, "writer");
            }
            _ => panic!("expected Dispatch"),
        }
        let n = state.nodes.get("n1").unwrap();
        assert_eq!(n.phase, TaskPhase::Planning);
    }

    #[test]
    fn plan_submitted_creates_subtasks_and_promotes_parent() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "root".into(),
                description: "Write a book".into(),
                spec: "10 chapters".into(),
                assigned_agent: "leader".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::PlanSubmitted {
                room_id: "room-1".into(),
                parent_node_id: "root".into(),
                subtasks: vec![
                    SubtaskSpec {
                        node_id: "ch1".into(),
                        description: "Chapter 1".into(),
                        spec: "1000 words".into(),
                        assigned_agent: "writer".into(),
                        sort_order: 0,
                    },
                    SubtaskSpec {
                        node_id: "ch2".into(),
                        description: "Chapter 2".into(),
                        spec: "1000 words".into(),
                        assigned_agent: "writer".into(),
                        sort_order: 1,
                    },
                ],
            },
        );
        assert_eq!(actions.len(), 2);
        assert_eq!(state.nodes.get("root").unwrap().phase, TaskPhase::Executing);
        assert_eq!(state.nodes.get("ch1").unwrap().phase, TaskPhase::Executing);
        assert_eq!(state.nodes.get("ch2").unwrap().phase, TaskPhase::Executing);
    }

    #[test]
    fn agent_output_advances_to_review() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "writer".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "writer".into(),
                success: true,
                output_excerpt: "done".into(),
            },
        );
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SchedulerAction::RequestReview { node_id, reviewer_id } => {
                assert_eq!(node_id, "n1");
                assert_eq!(reviewer_id, "__leader__");
            }
            _ => panic!("expected RequestReview"),
        }
        assert_eq!(state.nodes.get("n1").unwrap().phase, TaskPhase::Review);
        assert!(state.pending_reviews.contains_key("n1"));
    }

    #[test]
    fn review_pass_completes_node() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "writer".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "writer".into(),
                success: true,
                output_excerpt: "x".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::ReviewVerdict {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                result: ReviewResult {
                    passed: true,
                    comment: "good".into(),
                    reviewer_id: "__leader__".into(),
                    reviewed_at: Utc::now(),
                },
            },
        );
        // Pass emits one Persist (no Dispatch) so the runtime can
        // flush the new state (phase=Completed + review=passed) to
        // the subtasks table.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], SchedulerAction::Persist { .. }));
        assert_eq!(state.nodes.get("n1").unwrap().phase, TaskPhase::Completed);
        assert!(!state.pending_reviews.contains_key("n1"));
    }

    #[test]
    fn review_verdict_emits_persist() {
        // Every review verdict (pass OR fail) must emit a `Persist`
        // action so the runtime flushes the node's new state
        // (phase + review.comment + reviewer) to the subtasks table.
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "writer".into(),
            },
        );
        // --- failed review → must emit Dispatch + Persist ---
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::ReviewVerdict {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                result: ReviewResult {
                    passed: false,
                    comment: "needs more detail".into(),
                    reviewer_id: "__leader__".into(),
                    reviewed_at: Utc::now(),
                },
            },
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, SchedulerAction::Dispatch { .. })));
        let persist = actions
            .iter()
            .find(|a| matches!(a, SchedulerAction::Persist { .. }));
        assert!(
            persist.is_some(),
            "review verdict must emit Persist (got {:?})",
            actions
        );
        if let Some(SchedulerAction::Persist { room_id, node_id }) = persist {
            assert_eq!(room_id, "room-1");
            assert_eq!(node_id, "n1");
        }
    }

    #[test]
    fn review_fail_redispatches_for_revision() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "writer".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "writer".into(),
                success: true,
                output_excerpt: "x".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::ReviewVerdict {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                result: ReviewResult {
                    passed: false,
                    comment: "needs more detail".into(),
                    reviewer_id: "__leader__".into(),
                    reviewed_at: Utc::now(),
                },
            },
        );
        // Fail → Dispatch (re-dispatch to the same agent) + Persist
        // (flush new phase=Revising + review=failed). Order is
        // implementation-defined; we check both are present.
        assert_eq!(actions.len(), 2);
        assert!(actions
            .iter()
            .any(|a| matches!(a, SchedulerAction::Dispatch { node_id, agent_id }
                if node_id == "n1" && agent_id == "writer")));
        assert!(actions
            .iter()
            .any(|a| matches!(a, SchedulerAction::Persist { .. })));
        assert_eq!(state.nodes.get("n1").unwrap().phase, TaskPhase::Revising);
    }

    #[test]
    fn all_nodes_completed_emits_room_completed() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "a".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "w".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "b".into(),
                description: "y".into(),
                spec: "y".into(),
                assigned_agent: "w".into(),
            },
        );
        for id in ["a", "b"] {
            s.apply_event(
                &mut state,
                SchedulerEvent::AgentOutputSubmitted {
                    room_id: "room-1".into(),
                    node_id: id.into(),
                    agent_id: "w".into(),
                    success: true,
                    output_excerpt: "x".into(),
                },
            );
            let actions = s.apply_event(
                &mut state,
                SchedulerEvent::ReviewVerdict {
                    room_id: "room-1".into(),
                    node_id: id.into(),
                    result: ReviewResult {
                        passed: true,
                        comment: "ok".into(),
                        reviewer_id: "__leader__".into(),
                        reviewed_at: Utc::now(),
                    },
                },
            );
            if id == "b" {
                assert!(actions.iter().any(|a| matches!(a, SchedulerAction::RoomCompleted { .. })));
            }
        }
        assert!(state.all_completed);
    }

    #[test]
    fn spec_update_redispatches_revising_node() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "old".into(),
                assigned_agent: "w".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "w".into(),
                success: true,
                output_excerpt: "x".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::ReviewVerdict {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                result: ReviewResult {
                    passed: false,
                    comment: "redo".into(),
                    reviewer_id: "__leader__".into(),
                    reviewed_at: Utc::now(),
                },
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::SpecUpdated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                new_spec: "new requirement".into(),
            },
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], SchedulerAction::Dispatch { .. }));
        assert_eq!(state.nodes.get("n1").unwrap().spec, "new requirement");
    }

    #[test]
    fn cancel_single_node() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "a".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "w".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "b".into(),
                description: "y".into(),
                spec: "y".into(),
                assigned_agent: "w".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::Cancelled {
                room_id: "room-1".into(),
                node_id: Some("a".into()),
            },
        );
        assert!(actions.is_empty());
        assert_eq!(state.nodes.get("a").unwrap().phase, TaskPhase::Cancelled);
        assert_eq!(state.nodes.get("b").unwrap().phase, TaskPhase::Planning);
    }

    #[test]
    fn cancel_room_cancels_all() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "a".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "w".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "b".into(),
                description: "y".into(),
                spec: "y".into(),
                assigned_agent: "w".into(),
            },
        );
        s.apply_event(
            &mut state,
            SchedulerEvent::Cancelled {
                room_id: "room-1".into(),
                node_id: None,
            },
        );
        for n in state.nodes.values() {
            assert_eq!(n.phase, TaskPhase::Cancelled);
        }
    }

    #[test]
    fn score_change_to_zero_expels_agent() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::ScoreChanged {
                room_id: "room-1".into(),
                agent_id: "bad".into(),
                new_score: -10,
            },
        );
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SchedulerAction::AgentExpelled { agent_id, .. } => {
                assert_eq!(agent_id, "bad");
            }
            _ => panic!("expected AgentExpelled"),
        }
    }

    #[test]
    fn score_change_above_zero_is_noop() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::ScoreChanged {
                room_id: "room-1".into(),
                agent_id: "good".into(),
                new_score: 50,
            },
        );
        assert_eq!(actions, vec![SchedulerAction::Idle]);
    }

    #[test]
    fn wrong_room_id_is_ignored() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "other-room".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "w".into(),
            },
        );
        assert_eq!(actions, vec![SchedulerAction::Idle]);
        assert!(state.nodes.is_empty());
    }

    #[test]
    fn finish_all_mode_flag() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::FinishAllRequested {
                room_id: "room-1".into(),
            },
        );
        assert!(state.finish_all_mode);
    }

    #[test]
    fn agent_output_from_wrong_agent_is_ignored() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "writer".into(),
            },
        );
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "imposter".into(),
                success: true,
                output_excerpt: "x".into(),
            },
        );
        assert_eq!(actions, vec![SchedulerAction::Idle]);
        assert_eq!(state.nodes.get("n1").unwrap().phase, TaskPhase::Planning);
    }

    #[test]
    fn children_returns_sorted() {
        let mut state = fresh_state();
        state.nodes.insert(
            "root".into(),
            TaskNode {
                id: "root".into(),
                room_id: "room-1".into(),
                parent_id: None,
                phase: TaskPhase::Executing,
                description: "".into(),
                spec: "".into(),
                assigned_agent: "l".into(),
                output: "".into(),
                review: None,
                summary_a: None,
                summary_b: None,
                sort_order: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        for (id, order) in [("c1", 1), ("c0", 0), ("c2", 2)] {
            state.nodes.insert(
                id.into(),
                TaskNode {
                    id: id.into(),
                    room_id: "room-1".into(),
                    parent_id: Some("root".into()),
                    phase: TaskPhase::Executing,
                    description: "".into(),
                    spec: "".into(),
                    assigned_agent: "w".into(),
                    output: "".into(),
                    review: None,
                    summary_a: None,
                    summary_b: None,
                    sort_order: order,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
        }
        let ids: Vec<&str> = state.children("root").iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["c0", "c1", "c2"]);
    }

    #[test]
    fn agent_yield_stays_in_executing() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        s.apply_event(
            &mut state,
            SchedulerEvent::TaskCreated {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                description: "x".into(),
                spec: "x".into(),
                assigned_agent: "w".into(),
            },
        );
        // success=false → agent yielded, no review, no advancement.
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::AgentOutputSubmitted {
                room_id: "room-1".into(),
                node_id: "n1".into(),
                agent_id: "w".into(),
                success: false,
                output_excerpt: "need more info".into(),
            },
        );
        assert!(actions.is_empty());
        assert_eq!(state.nodes.get("n1").unwrap().phase, TaskPhase::Planning);
    }

    #[test]
    fn audit_verdict_warning_emits_penalty() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::AuditVerdict {
                room_id: "room-1".into(),
                agent_id: "writer".into(),
                violation_type: "废话文学".into(),
                severity: "warning".into(),
                deduction: 10,
                score_before: 100,
                score_after: 90,
                red_line: false,
                violation_id: "v-001".into(),
            },
        );
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SchedulerAction::IssuePenalty { agent_id, score_after, red_line, .. } => {
                assert_eq!(agent_id, "writer");
                assert_eq!(*score_after, 90);
                assert!(!red_line);
            }
            _ => panic!("expected IssuePenalty"),
        }
        assert_eq!(state.last_known_score.get("writer"), Some(&90));
    }

    #[test]
    fn audit_verdict_zero_score_emits_expel() {
        let s = TaskScheduler::new();
        let mut state = fresh_state();
        let actions = s.apply_event(
            &mut state,
            SchedulerEvent::AuditVerdict {
                room_id: "room-1".into(),
                agent_id: "writer".into(),
                violation_type: "弄虚作假".into(),
                severity: "red_line".into(),
                deduction: 500,
                score_before: 500,
                score_after: 0,
                red_line: true,
                violation_id: "v-red".into(),
            },
        );
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], SchedulerAction::IssuePenalty { ref agent_id, red_line: true, .. } if agent_id == "writer"));
        assert!(matches!(actions[1], SchedulerAction::AgentExpelled { ref agent_id, .. } if agent_id == "writer"));
    }
}
