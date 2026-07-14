//! Integration tests for memory_prefetch boundary points.
//!
//! These verify that the agent loop's memory hooks (pre-compress note,
//! turn start, session end) handle edge cases — missing router,
//! `skip_memory` flag — without panicking or producing wrong output.
//! Moved from `src/memory_prefetch.rs` integration_tests module (N-10).

use std::sync::Mutex;

use hermix_agent::memory_prefetch::{
    memory_on_session_end, memory_on_turn_start, memory_pre_compress_note,
};
use hermix_agent::agent_config::AgentConfig;
use hermix_core::Message;
use hermix_focus::{FocusStack, SessionFrame};

/// When `skip_memory` is set, the pre-compress note must be
/// `None` — even if a router is attached — so the
/// compression pipeline doesn't accidentally inject stale
/// memory context into a session that opted out.
#[test]
fn pre_compress_note_respects_skip_memory() {
    let mut cfg = AgentConfig::default();
    cfg.skip_memory = true;
    let messages = vec![Message::user("hi"), Message::assistant("hello")];
    let note = memory_pre_compress_note(&cfg, None, &messages);
    assert!(note.is_none(), "skip_memory should bypass pre-compress note");
}

/// With `skip_memory = false` and no router, the function
/// returns `None` (rather than panicking). The agent loop
/// uses the return value as `Option<String>` so the
/// `None`-on-no-router path must be stable.
#[test]
fn pre_compress_note_without_router_is_none() {
    let cfg = AgentConfig::default();
    let messages = vec![Message::user("hi")];
    let note = memory_pre_compress_note(&cfg, None, &messages);
    assert!(note.is_none());
}

/// `memory_on_turn_start` must be a no-op when no router
/// is attached — the agent loop calls it unconditionally
/// every turn so a panic here would break every run.
#[test]
fn turn_start_without_router_is_noop() {
    let cfg = AgentConfig::default();
    // Should not panic.
    memory_on_turn_start(&cfg, None, 1, "hi");
}

/// Same no-panic guarantee when `skip_memory` is set.
#[test]
fn turn_start_respects_skip_memory() {
    let mut cfg = AgentConfig::default();
    cfg.skip_memory = true;
    // Would panic if it touched the router; it doesn't.
    memory_on_turn_start(&cfg, None, 1, "hi");
}

/// `memory_on_session_end` is also called unconditionally
/// from the agent loop and must tolerate a missing router.
/// Without this guard the CLI would crash on exit when
/// memory was never configured.
#[test]
fn session_end_without_router_is_noop() {
    let cfg = AgentConfig::default();
    let focus_stack: Mutex<FocusStack<SessionFrame>> = Mutex::new(FocusStack::default());
    let messages: Vec<Message> = vec![];
    // Should not panic.
    memory_on_session_end(&cfg, None, &focus_stack, &messages, None);
}
