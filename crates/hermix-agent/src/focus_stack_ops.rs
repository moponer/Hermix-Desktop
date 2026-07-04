//! Focus-stack plumbing for the agent loop.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - `note_session_started` — push the per-session `SessionRoot` frame
//!   exactly once at `run()` start (idempotent).
//! - `note_delegation_start` — push a `Delegation` frame for each
//!   in-flight `delegate_task`.
//! - `note_delegation_end` — pop the matching `Delegation` frame on
//!   completion (topmost if id empty), finishing payload + clearing
//!   it so the next `pop` generates a `FocusConclusion`.
//! - `current_focus_depth` — return the live stack depth as `u8`.
//!
//! Popped `Cleared` frames generate a `FocusConclusion` that is
//! buffered in `pending_conclusions` until
//! `crate::memory_prefetch::drain_focus_conclusions` ships them to
//! the long-term memory backend.

use std::sync::Mutex;

use hermix_focus::{
    FocusFrame, FocusFrameId, FocusStack, SessionFrame, SessionFrameKind, SessionFrameStatus,
};

/// Push a `SessionRoot` frame for the current session. Idempotent
/// — a second call is a no-op if a root frame is already on the
/// stack. Callers should invoke this once at `run()` start.
pub fn note_session_started(focus_stack: &Mutex<FocusStack<SessionFrame>>, task: impl Into<String>) {
    let mut stack = match focus_stack.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if stack
        .frames()
        .iter()
        .any(|f| matches!(f.payload.kind, SessionFrameKind::SessionRoot))
    {
        return;
    }
    stack.push(FocusFrame::new(SessionFrame::root(task)));
}

/// Push a `Delegation` frame for a `delegate_task` invocation.
/// `sub_agent_id` is the UUID the orchestrator / sub-agent returns;
/// pass an empty string if not yet known (it will be matched by
/// depth in `note_delegation_end`).
///
/// Returns the frame id, or `None` if the lock is poisoned.
pub fn note_delegation_start(
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    delegate_depth: u32,
    task: impl Into<String>,
    sub_agent_id: impl Into<String>,
) -> Option<FocusFrameId> {
    let mut stack = focus_stack.lock().ok()?;
    let frame = FocusFrame::new(SessionFrame::delegation(task, sub_agent_id, delegate_depth));
    Some(stack.push(frame))
}

/// Pop the topmost `Delegation` frame whose payload matches
/// `sub_agent_id` (or the topmost delegation frame if
/// `sub_agent_id` is empty). The popped frame is `mark_clear`-ed
/// first so the next `pop` generates a `FocusConclusion`.
///
/// Returns the depth the popped frame was at, or `None` if no
/// matching frame was on the stack.
pub fn note_delegation_end(
    focus_stack: &Mutex<FocusStack<SessionFrame>>,
    sub_agent_id: &str,
    status: SessionFrameStatus,
) -> Option<u32> {
    let mut stack = focus_stack.lock().ok()?;
    // Find the target frame: topmost Delegation matching
    // sub_agent_id, or just the topmost Delegation if the id is empty.
    let target_idx = {
        let frames = stack.frames();
        let mut found: Option<usize> = None;
        for (i, f) in frames.iter().enumerate().rev() {
            if !matches!(f.payload.kind, SessionFrameKind::Delegation) {
                continue;
            }
            if sub_agent_id.is_empty() || f.payload.sub_agent_id == sub_agent_id {
                found = Some(i);
                break;
            }
        }
        found?
    };
    let popped_depth = stack.frames()[target_idx].payload.depth;
    // Finish the payload first (status + ended_at), then mark_clear
    // (idempotent) and pop.
    let target_id = stack.frames()[target_idx].id;
    if let Some(frame) = stack.get_mut(target_id) {
        frame.payload.finish(status);
        frame.mark_clear();
    }
    // The pop API only pops the top, so to pop an interior frame
    // we have to drain-and-rebuild. This is cheap because the
    // stack is shallow (one entry per in-flight delegate_task).
    let frames_after_target = stack.frames().len() - target_idx - 1;
    let mut stash: Vec<FocusFrame<SessionFrame>> = Vec::with_capacity(frames_after_target);
    for _ in 0..frames_after_target {
        stash.push(stack.pop().expect("stack non-empty by len arithmetic"));
    }
    // Discard the popped target frame: the relevant data (depth,
    // status, ended_at) was extracted into `popped_depth` and
    // `target_id` above, and `finish()` + `mark_clear()` already
    // ran before the pop.
    let _popped = stack.pop().expect("target frame exists");
    stash.reverse();
    for f in stash {
        stack.push(f);
    }
    Some(popped_depth)
}

/// Current depth of the focus stack (number of frames on it).
/// Returned as `u8` because the consciousness tracker's
/// `set_focus_depth` takes `u8` and the stack never grows past
/// `MAX_DELEGATE_DEPTH` (typically 5) in practice.
pub fn current_focus_depth(focus_stack: &Mutex<FocusStack<SessionFrame>>) -> u8 {
    focus_stack
        .lock()
        .map(|s| s.depth() as u8)
        .unwrap_or(0)
}