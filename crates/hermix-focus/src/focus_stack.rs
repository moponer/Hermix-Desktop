//! FocusStack — a 5-state frame stack inspired by BaiLongma's design.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 (BaiLongma 焦点栈)
//! + §五 Phase 8 (FocusStack 抽象).
//!
//! ## States
//!
//! Every frame is in exactly one of 5 states:
//!
//! - `Created`  — just created, not yet "active"
//! - `Pushed`   — a sub-task entered this frame (i.e. a child frame was
//!                pushed on top of it)
//! - `Kept`     — frame persisted without modification
//! - `Returned` — a sub-task returned to this frame (after `pop`)
//! - `Cleared`  — frame is being closed and converted to a `FocusConclusion`
//!
//! ## Lifecycle
//!
//! ```text
//!     new()
//!       │
//!       ▼
//!   Created ──push(child)──▶ Pushed
//!       │                       │
//!       │  no child pushed      │  child returns (pop)
//!       ▼                       ▼
//!     Kept ◀───── child returns ──── Returned
//!       │
//!       │  mark_clear()
//!       ▼
//!   Cleared ──pop()──▶ auto-attach FocusConclusion to new top
//!       │
//!       ▼
//!   (gone)
//! ```
//!
//! ## Type parameter
//!
//! `FocusStack<T>` is generic over the **payload** type. The frame metadata
//! (id, state, salience, timestamps) is always present; `T` is the
//! user-defined content. Use `()` for stacks that only care about metadata.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::salience::{salience, SalienceConfig};

pub type FocusFrameId = Uuid;

/// The 5 states a focus frame can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameState {
    Created,
    Pushed,
    Kept,
    Returned,
    Cleared,
}

impl FrameState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, FrameState::Cleared)
    }
}

/// A single frame on the focus stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusFrame<T: Clone> {
    pub id: FocusFrameId,
    pub state: FrameState,
    pub payload: T,
    pub salience: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// ID of the parent frame (None for the root frame).
    pub parent_id: Option<FocusFrameId>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Pin count — increments each time `pin()` is called.
    pub pin_count: u32,
    /// Touch count — increments each time `touch()` is called.
    pub touch_count: u32,
}

impl<T: Clone> FocusFrame<T> {
    /// Create a new frame in `Created` state with salience 0.5.
    pub fn new(payload: T) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            state: FrameState::Created,
            payload,
            salience: 0.5,
            created_at: now,
            updated_at: now,
            parent_id: None,
            tags: Vec::new(),
            pin_count: 0,
            touch_count: 0,
        }
    }

    /// Builder: set salience.
    pub fn with_salience(mut self, s: f32) -> Self {
        self.salience = s.clamp(0.0, 1.0);
        self
    }

    /// Builder: add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Builder: set parent.
    pub fn with_parent(mut self, parent: FocusFrameId) -> Self {
        self.parent_id = Some(parent);
        self
    }

    /// Pin the frame — increases pin_count and boosts salience to 1.0.
    pub fn pin(&mut self) {
        self.pin_count += 1;
        self.salience = 1.0;
        self.touch();
    }

    /// Touch the frame — increments touch_count and updates updated_at.
    pub fn touch(&mut self) {
        self.touch_count = self.touch_count.saturating_add(1);
        self.updated_at = Utc::now();
    }

    /// Mark the frame as cleared. Idempotent.
    pub fn mark_clear(&mut self) {
        self.state = FrameState::Cleared;
        self.touch();
    }

    /// Mark the frame as kept.
    pub fn mark_kept(&mut self) {
        self.state = FrameState::Kept;
        self.touch();
    }

    /// Mark the frame as pushed (a child was just added on top).
    pub fn mark_pushed(&mut self) {
        self.state = FrameState::Pushed;
        self.touch();
    }

    /// Mark the frame as returned (a child just popped).
    pub fn mark_returned(&mut self) {
        self.state = FrameState::Returned;
        self.touch();
    }

    /// H5: re-derive `self.salience` from the salience algorithm
    /// (recency × freq × pin) using the frame's `updated_at` (last
    /// touch), `touch_count`, and `pin_count`.
    /// /// `now` is the reference time — pass `Utc::now()` in production,
    /// a fixed timestamp in tests. `cfg` carries the weight
    /// configuration (see [`SalienceConfig::default`]).
    /// /// Why a separate method: the field `self.salience` is set by
    /// `new()` (0.5), `with_salience()` (manual override), and `pin()`
    /// (1.0). None of those are time-aware. The agent loop / router
    /// should call `recompute_salience` whenever a fresh value is
    /// needed (before persisting, before ranking, on conclusion).
    pub fn recompute_salience(&mut self, cfg: &SalienceConfig, now: DateTime<Utc>) {
        let s = salience(self.updated_at, self.touch_count, self.pin_count, now, cfg);
        self.salience = s.total;
    }

    /// Convenience: `recompute_salience` with `Utc::now()`.
    pub fn refresh_salience(&mut self, cfg: &SalienceConfig) {
        self.recompute_salience(cfg, Utc::now());
    }
}

/// The compressed summary auto-generated when a `Cleared` frame is popped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusConclusion {
    pub source_frame_id: FocusFrameId,
    pub text: String,
    pub salience: f32,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl FocusConclusion {
    /// Generate a natural-language summary from a frame's metadata.
    /// /// Produces a human-readable conclusion like:
    /// "讨论了用户偏好设置，涉及 3 个话题，被引用 5 次，标记为重要"
    /// instead of machine metadata like:
    /// "[conclusion id=X salience=0.50 tags=[...]]"
    pub fn from_frame<T: Clone>(frame: &FocusFrame<T>) -> Self {
        let tag_desc = if frame.tags.is_empty() {
            String::new()
        } else {
            format!("涉及 {}", frame.tags.join("、"))
        };
        let freq_desc = if frame.touch_count > 1 {
            format!("被引用 {} 次", frame.touch_count)
        } else {
            String::new()
        };
        let pin_desc = if frame.pin_count > 0 {
            format!("标记为重要（{} 次置顶）", frame.pin_count)
        } else {
            String::new()
        };

        let parts: Vec<&str> = [tag_desc.as_str(), freq_desc.as_str(), pin_desc.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect();
        let text = if parts.is_empty() {
            format!("记录了相关讨论内容")
        } else {
            format!("记录了相关讨论，{}", parts.join("，"))
        };

        Self {
            source_frame_id: frame.id,
            text,
            salience: frame.salience,
            tags: frame.tags.clone(),
            created_at: Utc::now(),
        }
    }

    /// H5: same as [`FocusConclusion::from_frame`] but recomputes
    /// the frame's salience from the algorithm first, so the
    /// conclusion's salience matches the recency × freq × pin
    /// formula. Use this when generating conclusions for
    /// long-term storage (so the stored salience is "current",
    /// not whatever it was when the frame was last persisted).
    pub fn from_frame_with_cfg<T: Clone>(
        frame: &mut FocusFrame<T>,
        cfg: &SalienceConfig,
    ) -> Self {
        frame.recompute_salience(cfg, Utc::now());
        Self::from_frame(frame)
    }

    /// Builder: override text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }
}

#[derive(Debug, Error)]
pub enum FocusStackError {
    #[error("stack is empty")]
    Empty,
    #[error("frame id {0} not found")]
    NotFound(Uuid),
}

/// A generic 5-state focus frame stack.
///
/// The stack is **in-memory only** by default. Persist via `to_snapshot()`
/// / `from_snapshot()` for cross-process continuity, or pipe it through
/// the `MemoryLayer` trait for long-term storage.
#[derive(Debug, Clone)]
pub struct FocusStack<T: Clone> {
    frames: Vec<FocusFrame<T>>,
    by_id: HashMap<FocusFrameId, usize>,
    /// All conclusions auto-generated on `pop` of a `Cleared` frame.
    /// In a real system this is fed to the `MemoryLayer::recognize`
    /// pipeline for long-term indexing.
    pub pending_conclusions: Vec<FocusConclusion>,
}

impl<T: Clone> Default for FocusStack<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> FocusStack<T> {
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            by_id: HashMap::new(),
            pending_conclusions: Vec::new(),
        }
    }

    /// Push a new frame on top. Returns the frame id.
    /// If there was already a top frame, it's transitioned to `Pushed`
    /// — but only if it is **not** already in a terminal state
    /// (`Cleared`). `Cleared` is terminal and must not be overwritten
    /// by a transition; doing so would lose the "ready to be popped
    /// and become a conclusion" signal.
    pub fn push(&mut self, mut frame: FocusFrame<T>) -> FocusFrameId {
        // The current top (if any) becomes Pushed — unless it's
        // already terminal (Cleared), in which case we leave it
        // alone so the next `pop` can still generate a conclusion.
        if let Some(top_idx) = self.frames.len().checked_sub(1) {
            let top = &mut self.frames[top_idx];
            if !top.state.is_terminal() {
                top.mark_pushed();
            }
        }
        frame.parent_id = self.frames.last().map(|f| f.id);
        let id = frame.id;
        self.by_id.insert(id, self.frames.len());
        self.frames.push(frame);
        id
    }

    /// Peek at the top frame without removing it.
    pub fn peek(&self) -> Option<&FocusFrame<T>> {
        self.frames.last()
    }

    /// Mutable peek.
    pub fn peek_mut(&mut self) -> Option<&mut FocusFrame<T>> {
        self.frames.last_mut()
    }

    /// Pop the top frame.
    /// /// Behavior:
    /// - If the popped frame's state is `Cleared`, a `FocusConclusion`
    /// is auto-generated and stored in `pending_conclusions`. The
    /// new top (if any) is transitioned to `Returned` — but only
    /// if it is **not** already in a terminal state (`Cleared`).
    /// `Cleared` is terminal and must not be overwritten.
    /// - Otherwise, the frame is removed without conclusion generation,
    /// and the new top (if any) is transitioned to `Returned` (with
    /// the same terminal-state guard).
    /// - If the stack is empty, returns `Err(FocusStackError::Empty)`.
    pub fn pop(&mut self) -> Result<FocusFrame<T>, FocusStackError> {
        let frame = self.frames.pop().ok_or(FocusStackError::Empty)?;
        self.by_id.remove(&frame.id);
        // Auto-generate a conclusion if the frame was Cleared.
        if frame.state == FrameState::Cleared {
            self.pending_conclusions
                .push(FocusConclusion::from_frame(&frame));
        }
        // The new top is now "Returned" (from the just-popped
        // child) — but if it is already terminal (Cleared), leave
        // it alone so its own pop still generates a conclusion.
        if let Some(top_idx) = self.frames.len().checked_sub(1) {
            let top = &mut self.frames[top_idx];
            if !top.state.is_terminal() {
                top.mark_returned();
            }
        }
        Ok(frame)
    }

    /// Number of frames currently on the stack.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Look up a frame by id (mutable).
    pub fn get_mut(&mut self, id: FocusFrameId) -> Option<&mut FocusFrame<T>> {
        let idx = *self.by_id.get(&id)?;
        self.frames.get_mut(idx)
    }

    /// Look up a frame by id.
    pub fn get(&self, id: FocusFrameId) -> Option<&FocusFrame<T>> {
        let idx = *self.by_id.get(&id)?;
        self.frames.get(idx)
    }

    /// Drain pending conclusions, returning them and clearing the list.
    /// Callers should pipe these into a `MemoryLayer` for persistence.
    pub fn drain_conclusions(&mut self) -> Vec<FocusConclusion> {
        std::mem::take(&mut self.pending_conclusions)
    }

    /// Append a conclusion externally (e.g. from a sub-agent's stack).
    pub fn append_conclusion(&mut self, c: FocusConclusion) {
        self.pending_conclusions.push(c);
    }

    /// Return frames in current order (bottom → top).
    pub fn frames(&self) -> &[FocusFrame<T>] {
        &self.frames
    }

    /// Take a serializable snapshot of the current stack.
    pub fn to_snapshot(&self) -> FocusStackSnapshot<T> {
        FocusStackSnapshot {
            frames: self.frames.clone(),
            pending_conclusions: self.pending_conclusions.clone(),
        }
    }

    /// H5: re-derive salience on every frame on the stack using the
    /// supplied config. Useful right before persisting the snapshot
    /// or right before a recall — the algorithm assumes a "now"
    /// that drifts, so calling this periodically keeps the field
    /// honest.
    pub fn refresh_salience_all(&mut self, cfg: &SalienceConfig) {
        let now = Utc::now();
        for f in &mut self.frames {
            f.recompute_salience(cfg, now);
        }
    }

    /// Restore a stack from a snapshot.
    pub fn from_snapshot(snap: FocusStackSnapshot<T>) -> Self {
        let mut by_id = HashMap::new();
        for (i, f) in snap.frames.iter().enumerate() {
            by_id.insert(f.id, i);
        }
        Self {
            frames: snap.frames,
            by_id,
            pending_conclusions: snap.pending_conclusions,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusStackSnapshot<T: Clone> {
    pub frames: Vec<FocusFrame<T>>,
    pub pending_conclusions: Vec<FocusConclusion>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stack_is_empty() {
        let s: FocusStack<String> = FocusStack::new();
        assert_eq!(s.depth(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn push_increments_depth() {
        let mut s: FocusStack<String> = FocusStack::new();
        let id = s.push(FocusFrame::new("a".to_string()));
        assert_eq!(s.depth(), 1);
        assert!(s.get(id).is_some());
    }

    #[test]
    fn push_marks_old_top_as_pushed() {
        let mut s: FocusStack<String> = FocusStack::new();
        let a = s.push(FocusFrame::new("a".to_string()));
        s.push(FocusFrame::new("b".to_string()));
        assert_eq!(s.get(a).unwrap().state, FrameState::Pushed);
    }

    #[test]
    fn pop_empty_errors() {
        let mut s: FocusStack<String> = FocusStack::new();
        assert!(s.pop().is_err());
    }

    #[test]
    fn pop_non_cleared_no_conclusion() {
        let mut s: FocusStack<String> = FocusStack::new();
        s.push(FocusFrame::new("a".to_string()));
        s.pop().unwrap();
        assert!(s.pending_conclusions.is_empty());
    }

    #[test]
    fn pop_cleared_generates_conclusion() {
        let mut s: FocusStack<String> = FocusStack::new();
        let id = s.push(FocusFrame::new("a".to_string()).with_tag("test"));
        s.get_mut(id).unwrap().mark_clear();
        s.pop().unwrap();
        assert_eq!(s.pending_conclusions.len(), 1);
        let c = &s.pending_conclusions[0];
        assert_eq!(c.source_frame_id, id);
        assert!(c.text.contains("test"));
    }

    #[test]
    fn pop_marks_new_top_as_returned() {
        let mut s: FocusStack<String> = FocusStack::new();
        let a = s.push(FocusFrame::new("a".to_string()));
        s.push(FocusFrame::new("b".to_string()));
        s.pop().unwrap();
        assert_eq!(s.get(a).unwrap().state, FrameState::Returned);
    }

    #[test]
    fn pin_boosts_salience_to_one() {
        let mut f = FocusFrame::new("x");
        assert!(f.salience < 1.0);
        f.pin();
        assert_eq!(f.salience, 1.0);
        assert_eq!(f.pin_count, 1);
    }

    #[test]
    fn touch_increments_count() {
        let mut f = FocusFrame::new("x");
        assert_eq!(f.touch_count, 0);
        f.touch();
        f.touch();
        assert_eq!(f.touch_count, 2);
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut s: FocusStack<String> = FocusStack::new();
        s.push(FocusFrame::new("a".to_string()));
        s.push(FocusFrame::new("b".to_string()));
        let snap = s.to_snapshot();
        let restored: FocusStack<String> = FocusStack::from_snapshot(snap);
        assert_eq!(restored.depth(), 2);
        assert_eq!(restored.frames()[0].payload, "a");
        assert_eq!(restored.frames()[1].payload, "b");
    }

    #[test]
    fn frame_with_salience_clamps() {
        let f = FocusFrame::new("x").with_salience(99.0);
        assert_eq!(f.salience, 1.0);
        let f = FocusFrame::new("x").with_salience(-1.0);
        assert_eq!(f.salience, 0.0);
    }

    #[test]
    fn drain_conclusions_clears_list() {
        let mut s: FocusStack<String> = FocusStack::new();
        let id = s.push(FocusFrame::new("a".to_string()));
        s.get_mut(id).unwrap().mark_clear();
        s.pop().unwrap();
        assert_eq!(s.pending_conclusions.len(), 1);
        let drained = s.drain_conclusions();
        assert_eq!(drained.len(), 1);
        assert!(s.pending_conclusions.is_empty());
    }

    #[test]
    fn child_frame_has_parent_id() {
        let mut s: FocusStack<String> = FocusStack::new();
        let a = s.push(FocusFrame::new("a".to_string()));
        let b = s.push(FocusFrame::new("b".to_string()));
        assert_eq!(s.get(b).unwrap().parent_id, Some(a));
    }

    // ── H5: salience recompute ──

    /// A frame touched many times beats a frame touched once, all
    /// other things equal — and the difference is algorithm-driven,
    /// not field-equal.
    #[test]
    fn recompute_uses_touch_count() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let mut low = FocusFrame::new("low");
        low.touch();
        low.recompute_salience(&cfg, now);
        let mut high = FocusFrame::new("high");
        for _ in 0..20 {
            high.touch();
        }
        high.recompute_salience(&cfg, now);
        assert!(
            high.salience > low.salience,
            "high ({}) should beat low ({})",
            high.salience,
            low.salience
        );
    }

    /// Pinning a frame elevates it through the pin_score channel —
    /// the recomputed salience is meaningfully higher than an
    /// unpinned sibling with the same touch history.
    #[test]
    fn recompute_uses_pin_count() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let mut unpinned = FocusFrame::new("u");
        unpinned.touch();
        unpinned.recompute_salience(&cfg, now);
        let mut pinned = FocusFrame::new("p");
        pinned.touch();
        pinned.pin();
        pinned.recompute_salience(&cfg, now);
        assert!(
            pinned.salience > unpinned.salience,
            "pinned ({}) should beat unpinned ({})",
            pinned.salience,
            unpinned.salience
        );
    }

    /// A frame updated recently beats a frame updated a week ago,
    /// all other things equal — proves the recency channel is wired.
    /// Set `updated_at` directly so the test doesn't depend on the
    /// current wall clock.
    #[test]
    fn recompute_uses_recency() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let mut stale = FocusFrame::new("old");
        stale.updated_at = now - chrono::Duration::days(7);
        stale.recompute_salience(&cfg, now);
        let mut fresh = FocusFrame::new("new");
        fresh.updated_at = now;
        fresh.recompute_salience(&cfg, now);
        assert!(
            fresh.salience > stale.salience,
            "fresh ({}) should beat stale ({})",
            fresh.salience,
            stale.salience
        );
    }

    /// `from_frame_with_cfg` recomputes before snapshotting the
    /// salience into the conclusion. Verifies by manually setting
    /// a stale `salience` and confirming it gets overridden.
    #[test]
    fn conclusion_with_cfg_overrides_stale_field() {
        let cfg = SalienceConfig::default();
        let mut f = FocusFrame::new("c");
        // Manually set a stale salience — this is what the DB
        // would give us if we'd loaded an old frame back.
        f.salience = 0.01;
        f.touch();
        f.touch();
        let c = FocusConclusion::from_frame_with_cfg(&mut f, &cfg);
        // The conclusion should NOT carry the stale 0.01 — it
        // should carry the freshly-recomputed value.
        assert!(
            c.salience > 0.01,
            "conclusion salience ({}) should be > stale 0.01",
            c.salience
        );
    }

    /// `refresh_salience_all` walks every frame on the stack. The
    /// recency channel has to fire — frames on the stack longer
    /// should be less salient than a just-pushed one.
    /// /// We push *both* frames first, then backdate the first one's
    /// `updated_at`. (If we backdated before the second push, the
    /// second push would `touch()` the first frame via `mark_pushed`,
    /// overwriting our backdate with wall-clock now.)
    #[test]
    fn refresh_salience_all_recomputes_every_frame() {
        let cfg = SalienceConfig::default();
        let mut s: FocusStack<String> = FocusStack::new();
        let old_id = s.push(FocusFrame::new("old".to_string()));
        s.push(FocusFrame::new("new".to_string()));
        // Now backdate the bottom frame to 30 days ago.
        s.get_mut(old_id).unwrap().updated_at =
            Utc::now() - chrono::Duration::days(30);
        s.get_mut(old_id).unwrap().salience = 0.99; // stale max
        // Refresh — the algorithm should drop the bottom frame's
        // salience to near zero.
        s.refresh_salience_all(&cfg);
        let old = s.get(old_id).unwrap().salience;
        let new = s.frames().last().unwrap().salience;
        assert!(
            new > old,
            "fresh ({}) should beat backdated ({}) after refresh",
            new,
            old
        );
    }

    /// Re-running `recompute_salience` on the *same* frame at the
    /// same `now` is idempotent — calling it twice in a row
    /// produces the same value.
    #[test]
    fn recompute_is_idempotent_for_same_now() {
        let cfg = SalienceConfig::default();
        let now = Utc::now();
        let mut f = FocusFrame::new("x");
        f.touch();
        f.touch();
        f.recompute_salience(&cfg, now);
        let v1 = f.salience;
        f.recompute_salience(&cfg, now);
        let v2 = f.salience;
        assert!((v1 - v2).abs() < 1e-6, "{v1} != {v2}");
    }
}
