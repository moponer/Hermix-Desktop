//! SessionFrame — the canonical payload type for [`FocusStack`] in the
//! agent loop.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 (BaiLongma 焦点栈) +
//! ("FocusStack 抽象").
//!
//! Each top-level session is rooted in one `SessionFrame`; every
//! `delegate_task` invocation pushes a child frame; when the sub-agent
//! returns the frame is `Cleared` and popped, generating a
//! [`FocusConclusion`] that is fed back to long-term memory.
//!
//! `SessionFrame` is the "user-defined content" half of
//! `FocusStack<SessionFrame>` — frame metadata (state, salience, ids,
//! timestamps) lives in [`FocusFrame`], payload lives here. This split
//! keeps the data structure generic and reusable for other payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The "kind" of frame on the focus stack.
///
/// Today the agent loop only really exercises `Delegation` (one frame
/// per `delegate_task` tool call). We keep the enum open so future
/// callers (e.g. sub-tool calls, user turns) can branch on it without
/// a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionFrameKind {
    /// Frame anchored to the whole user session. Always at the bottom
    /// of the stack; pushed by `AgentLoop::run()` start.
    SessionRoot,
    /// Frame for a `delegate_task` sub-agent invocation.
    Delegation,
    /// Frame for a long-running tool call (e.g. voice_mode turn).
    /// Reserved for future use; the agent loop does not push these yet.
    LongTool,
}

/// Terminal status of a frame when it was popped.
///
/// Mirrors `FrameState::Cleared` (the only state from which `pop`
/// generates a conclusion), but records the *outcome* of the
/// sub-task, not the frame state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionFrameStatus {
    /// Sub-task completed normally.
    Completed,
    /// Sub-task returned an error.
    Failed,
    /// Sub-task was cancelled (interrupt, timeout, or depth cap).
    Cancelled,
}

/// The user-defined payload of a [`FocusFrame`] on the agent loop's
/// focus stack. See module docs for the role split between
/// `FocusFrame` (metadata) and `SessionFrame` (content).
///
/// All fields are `pub` so the agent loop can build it incrementally
/// (start → set `ended_at` + `status` on completion) without a
/// builder ceremony. The struct is `Clone + Serialize + Deserialize`
/// so it can ride through `FocusStackSnapshot` for cross-process
/// continuity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFrame {
    /// What kind of frame this is — see [`SessionFrameKind`].
    pub kind: SessionFrameKind,
    /// The natural-language task / topic this frame is about. For
    /// `Delegation` frames, this is the `task` argument the LLM
    /// passed to `delegate_task`. Free-form, not structured.
    pub task: String,
    /// The sub-agent id (UUID) — empty for `SessionRoot` frames.
    /// Matches `sub_agent_id` in the `delegate_task` tool result.
    pub sub_agent_id: String,
    /// When the frame was pushed. Mirrors `FocusFrame::created_at`
    /// but lives on the payload side for snapshotting.
    pub started_at: DateTime<Utc>,
    /// When the frame was popped (set on `note_delegation_end`,
    /// `None` while the frame is still on the stack).
    pub ended_at: Option<DateTime<Utc>>,
    /// Terminal status — `None` while still on the stack, set when
    /// the frame transitions to `Cleared`.
    pub status: Option<SessionFrameStatus>,
    /// Recursion depth at the time of push (0 = root, 1 = child, …).
    /// Mirrors `AgentLoop::delegate_depth` at push time, so a
    /// snapshot stays meaningful even if the loop is restarted.
    pub depth: u32,
}

impl SessionFrame {
    /// Convenience: build a `SessionRoot` frame at depth 0.
    pub fn root(task: impl Into<String>) -> Self {
        Self {
            kind: SessionFrameKind::SessionRoot,
            task: task.into(),
            sub_agent_id: String::new(),
            started_at: Utc::now(),
            ended_at: None,
            status: None,
            depth: 0,
        }
    }

    /// Convenience: build a `Delegation` frame for a `delegate_task`
    /// invocation. `depth` is the recursion depth the delegation
    /// happens at (i.e. the parent's `delegate_depth + 1`).
    pub fn delegation(task: impl Into<String>, sub_agent_id: impl Into<String>, depth: u32) -> Self {
        Self {
            kind: SessionFrameKind::Delegation,
            task: task.into(),
            sub_agent_id: sub_agent_id.into(),
            started_at: Utc::now(),
            ended_at: None,
            status: None,
            depth,
        }
    }

    /// Mark the frame as finished. Called from
    /// `note_delegation_end` before `mark_clear` / `pop`.
    pub fn finish(&mut self, status: SessionFrameStatus) {
        self.ended_at = Some(Utc::now());
        self.status = Some(status);
    }

    /// Convenience for short human-readable labels. Used in the
    /// default `FocusConclusion::from_frame` text and in
    /// `tracing::debug!` breadcrumbs.
    pub fn label(&self) -> String {
        let kind = match self.kind {
            SessionFrameKind::SessionRoot => "session",
            SessionFrameKind::Delegation => "delegate",
            SessionFrameKind::LongTool => "tool",
        };
        let task = if self.task.len() > 40 {
            format!("{}…", &self.task[..40])
        } else {
            self.task.clone()
        };
        if self.sub_agent_id.is_empty() {
            format!("[{kind}] {task}")
        } else {
            format!("[{kind}] {task} (sub={})", &self.sub_agent_id[..8.min(self.sub_agent_id.len())])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_has_no_sub_agent_id() {
        let f = SessionFrame::root("user session");
        assert_eq!(f.kind, SessionFrameKind::SessionRoot);
        assert_eq!(f.depth, 0);
        assert!(f.sub_agent_id.is_empty());
        assert!(f.ended_at.is_none());
        assert!(f.status.is_none());
    }

    #[test]
    fn delegation_captures_task_and_sub_id() {
        let f = SessionFrame::delegation("summarize docs", "sub-abc-123", 1);
        assert_eq!(f.kind, SessionFrameKind::Delegation);
        assert_eq!(f.depth, 1);
        assert_eq!(f.sub_agent_id, "sub-abc-123");
        assert!(f.ended_at.is_none());
    }

    #[test]
    fn finish_sets_ended_at_and_status() {
        let mut f = SessionFrame::delegation("t", "s", 1);
        assert!(f.ended_at.is_none());
        f.finish(SessionFrameStatus::Completed);
        assert!(f.ended_at.is_some());
        assert_eq!(f.status, Some(SessionFrameStatus::Completed));
    }

    #[test]
    fn label_truncates_long_tasks() {
        let long = "a".repeat(80);
        let f = SessionFrame::root(long);
        let l = f.label();
        assert!(l.contains("…"));
        assert!(l.len() < 60);
    }

    #[test]
    fn label_includes_short_sub_id() {
        let f = SessionFrame::delegation("do thing", "abcdef12-3456", 1);
        let l = f.label();
        assert!(l.contains("[delegate]"));
        assert!(l.contains("abcdef12"));
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let mut f = SessionFrame::delegation("task X", "sub-1", 2);
        f.finish(SessionFrameStatus::Failed);
        let json = serde_json::to_string(&f).unwrap();
        let back: SessionFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, f.kind);
        assert_eq!(back.task, f.task);
        assert_eq!(back.sub_agent_id, f.sub_agent_id);
        assert_eq!(back.depth, f.depth);
        assert_eq!(back.status, f.status);
        assert!(back.ended_at.is_some());
    }

    #[test]
    fn snapshot_via_focus_stack_round_trip() {
        use crate::focus_stack::FocusFrame;
        let mut stack: crate::focus_stack::FocusStack<SessionFrame> =
            crate::focus_stack::FocusStack::new();
        stack.push(FocusFrame::new(SessionFrame::root("session 1")));
        stack.push(FocusFrame::new(SessionFrame::delegation(
            "child task",
            "child-1",
            1,
        )));
        let snap = stack.to_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back_snap: crate::focus_stack::FocusStackSnapshot<SessionFrame> =
            serde_json::from_str(&json).unwrap();
        let restored = crate::focus_stack::FocusStack::from_snapshot(back_snap);
        assert_eq!(restored.depth(), 2);
        assert_eq!(restored.frames()[0].payload.task, "session 1");
        assert_eq!(restored.frames()[1].payload.sub_agent_id, "child-1");
        assert_eq!(restored.frames()[1].payload.depth, 1);
    }

    //── lifecycle contract tests ──
    // These exercise the full push → finish → mark_clear → pop →
    // drain flow that AgentLoop::note_delegation_start +
    // note_delegation_end + drain_focus_conclusions_to_memory_router
    // will run in production. They prove the hermix-focus
    // primitives are enough; the hermix-agent glue tests are
    // blocked by an unrelated pre-existing hermix-tools build
    // break (enigo API mismatch) and live in a follow-up.

    #[test]
    fn delegation_lifecycle_happy_path_generates_conclusion() {
        use crate::focus_stack::{FocusFrame, FocusStack, FrameState};

        let mut stack: FocusStack<SessionFrame> = FocusStack::new();
        // 1. Session root
        let root_id = stack.push(FocusFrame::new(SessionFrame::root("user task")));
        assert_eq!(stack.depth(), 1);
        // 2. Sub-agent delegation push
        let child_id =
            stack.push(FocusFrame::new(SessionFrame::delegation("summarize", "sub-1", 1)));
        assert_eq!(stack.depth(), 2);
        // 3. Root frame is now `Pushed` (parent of the new child)
        assert_eq!(stack.get(root_id).unwrap().state, FrameState::Pushed);
        // 4. Sub-agent finishes — finish the payload + mark_clear
        let child = stack.get_mut(child_id).unwrap();
        child.payload.finish(SessionFrameStatus::Completed);
        child.mark_clear();
        assert_eq!(child.state, FrameState::Cleared);
        // 5. Pop the child — should auto-generate a conclusion
        let popped = stack.pop().unwrap();
        assert_eq!(popped.id, child_id);
        assert_eq!(popped.payload.status, Some(SessionFrameStatus::Completed));
        assert!(popped.payload.ended_at.is_some());
        // 6. The pending conclusion is sitting in the stack
        assert_eq!(stack.pending_conclusions.len(), 1);
        let c = &stack.pending_conclusions[0];
        assert_eq!(c.source_frame_id, child_id);
        // 7. Drain — clears the pending list, returns the conclusion
        let drained = stack.drain_conclusions();
        assert_eq!(drained.len(), 1);
        assert!(stack.pending_conclusions.is_empty());
        // 8. Root frame is now `Returned`
        assert_eq!(stack.get(root_id).unwrap().state, FrameState::Returned);
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn delegation_lifecycle_failed_status_propagates() {
        use crate::focus_stack::{FocusFrame, FocusStack};

        let mut stack: FocusStack<SessionFrame> = FocusStack::new();
        stack.push(FocusFrame::new(SessionFrame::root("r")));
        let cid = stack.push(FocusFrame::new(SessionFrame::delegation("t", "sub-x", 1)));
        {
            let f = stack.get_mut(cid).unwrap();
            f.payload.finish(SessionFrameStatus::Failed);
            f.mark_clear();
        }
        let popped = stack.pop().unwrap();
        assert_eq!(popped.payload.status, Some(SessionFrameStatus::Failed));
        let drained = stack.drain_conclusions();
        // The conclusion carries the failed status via the
        // payload snapshot — the FocusConclusion struct itself
        // doesn't have a `status` field, but the source frame
        // does and `from_frame_with_cfg` re-derives salience.
        assert_eq!(drained[0].source_frame_id, cid);
    }

    #[test]
    fn parallel_delegations_pop_in_lifo_order() {
        use crate::focus_stack::{FocusFrame, FocusStack, FrameState};

        // Two sub-agents pushed in parallel; the topmost must be
        // the first one popped (the agent loop's normal
        // LIFO assumption).
        let mut stack: FocusStack<SessionFrame> = FocusStack::new();
        stack.push(FocusFrame::new(SessionFrame::root("r")));
        let a = stack.push(FocusFrame::new(SessionFrame::delegation("A", "sub-a", 1)));
        let b = stack.push(FocusFrame::new(SessionFrame::delegation("B", "sub-b", 1)));
        assert_eq!(stack.depth(), 3);
        // Mark both cleared (no loop — clearer for the borrow
        // checker and easier to read failure output).
        {
            let fa = stack.get_mut(a).unwrap();
            fa.payload.finish(SessionFrameStatus::Completed);
            fa.mark_clear();
        }
        {
            let fb = stack.get_mut(b).unwrap();
            fb.payload.finish(SessionFrameStatus::Completed);
            fb.mark_clear();
        }
        // Sanity: both frames are now in `Cleared` state.
        assert_eq!(stack.get(a).unwrap().state, FrameState::Cleared);
        assert_eq!(stack.get(b).unwrap().state, FrameState::Cleared);
        let popped_b = stack.pop().unwrap();
        assert_eq!(popped_b.id, b);
        let popped_a = stack.pop().unwrap();
        assert_eq!(popped_a.id, a);
        // Both should have generated conclusions.
        let drained = stack.drain_conclusions();
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn focus_conclusion_uses_algorithm_salience_via_with_cfg() {
        use crate::focus_stack::{FocusFrame, FocusStack};
        use crate::salience::SalienceConfig;

        let mut stack: FocusStack<SessionFrame> = FocusStack::new();
        stack.push(FocusFrame::new(SessionFrame::root("r")));
        let cid = stack.push(FocusFrame::new(SessionFrame::delegation("t", "sub-1", 1)));
        {
            let f = stack.get_mut(cid).unwrap();
            f.touch();
            f.touch();
            f.touch();
        }
        // Simulate "the frame was loaded back from DB with a
        // stale 0.01 salience" — exactly what H5 left for us.
        stack.get_mut(cid).unwrap().salience = 0.01;
        let mut frame = stack.get_mut(cid).unwrap().clone();
        let conclusion = crate::focus_stack::FocusConclusion::from_frame_with_cfg(
            &mut frame,
            &SalienceConfig::default(),
        );
        assert!(
            conclusion.salience > 0.01,
            "with_cfg should override stale 0.01 with algorithm result, got {}",
            conclusion.salience
        );
    }
}
