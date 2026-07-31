//! # hermix-focus
//!
//! H5: Unified memory layer + focus stack + salience scoring.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 + §3 H5 + §五 .
//!
//! ## What this crate provides
//!
//! 1. **[`MemoryLayer`] trait** — a single, backend-agnostic interface for any
//!    memory provider (8 external SaaS plugins + 1 local embedded backend).
//!    Replaces the per-plugin `MemoryProviderPlugin` zoo with one type.
//!
//! 2. **[`FocusStack<T>`]** — a generic, type-parameterized frame stack with
//!    the 5 states described in the BaiLongma article:
//!    - `Created` (刚创建)
//!    - `Pushed` (子任务进入)
//!    - `Kept` (保持)
//!    - `Returned` (子任务返回)
//!    - `Cleared` (清理为结论)
//!    On `pop` of a `Cleared` frame, a `focus_conclusion` is auto-generated
//!    and attached to the new top of the stack, then optionally persisted
//!    to long-term memory.
//!
//! 3. **[`FocusFrame`] / [`FocusConclusion`]** — concrete frame + summary
//!    types the stack operates on. Frames carry salience, timestamps,
//!    parent links, and a free-form payload.
//!
//! 4. **[`salience`]** — a salience scoring algorithm. Combines:
//!    - Recency (time-decay)
//!    - Frequency (how many times the frame was touched)
//!    - Explicit boost (user `pin`)
//!    Returns a normalized `[0.0, 1.0]` score.
//!
//! 5. **[`EmbeddedMemoryBackend`]** — a SQLite-backed implementation of
//!    `MemoryLayer` that lives entirely in `hermix_home/data/memory.db`.
//!    This is the "本地优先" backend the调研 recommended.
//!
//! 6. **[`MemoryRouter`]** — a `MemoryLayer` impl that fans out to N
//!    underlying `MemoryLayer`s, deduplicates hits, and re-ranks by salience.
//!    Drop-in replacement for the existing 8-plugin switchboard.
//!
//! 7. **[`TimeAwareRecall`]** — extracts Chinese/English time expressions
//!    ("昨天", "上周", "yesterday", "last week") from the query and filters
//!    recalled frames to the matching time window.
//!
//! ## Quick start
//!
//! ```
//! use hermix_focus::{FocusStack, FocusFrame};
//!
//! let mut stack: FocusStack<String> = FocusStack::new();
//! let _ = stack.push(FocusFrame::new("Wrote hermix-focus crate".to_string()));
//! assert_eq!(stack.depth(), 1);
//! ```

#![allow(clippy::needless_range_loop)]

pub mod embedded;
pub mod fact_extractor;
pub mod user_style;
pub mod focus_stack;
pub mod memory_layer;
pub mod memory_router;
pub mod salience;
pub mod session_frame;
pub mod time_aware_recall;
pub mod time_recall;

pub use embedded::EmbeddedMemoryBackend;
pub use embedded::fact_ttl_for_key;
pub use focus_stack::{FocusConclusion, FocusFrame, FocusFrameId, FocusStack, FrameState};
pub use session_frame::{SessionFrame, SessionFrameKind, SessionFrameStatus};
pub use memory_layer::{
    DefaultPromptFormatter, DynMemoryLayer, HealthStatus, Intent, MemoryError, MemoryHit,
    MemoryLayer, MemoryQuery, Result, SystemPromptBlock, SystemPromptFormatter, TimeWindow,
};
pub use memory_router::{MemoryRouter, RouterConfig};
pub use salience::{salience, SalienceComponents, SalienceConfig};
pub use time_aware_recall::{RecallHint, RecallIntent, RecallSpan, TimeAwareRecall};
pub use time_recall::{extract_time_window, TimeExpression};
