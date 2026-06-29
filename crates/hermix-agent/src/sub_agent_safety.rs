//! Sub-agent safety: tool blacklist + depth cap enforcement.
//!
//! Inspired by the upstream Hermes Python project's
//! `DELEGATE_BLOCKED_TOOLS` constant (sub-agents cannot
//! recursively delegate, cannot interact with the user, cannot
//! write to shared memory, and cannot send messages to
//! external platforms) and the `MAX_DELEGATE_DEPTH = 2` cap.
//!
//! Implemented in Rust as a single auditable module that
//! exposes:
//!
//!   - [`BLOCKED_TOOLS_FOR_SUBAGENT`]: the hard-coded list of
//!     tool names a `delegate_task` child is NEVER allowed to
//!     call. The list is checked both:
//!     (a) at dispatch time, so the LLM's `delegate_task`
//!         call with a child toolset that includes a blocked
//!         name gets a `Deny` verdict with a clear reason;
//!     (b) at child agent construction, so the child's
//!         `ToolRegistry` is constructed with the blocked
//!         tools stripped — even a misbehaving LLM can't call
//!         them because the handler is gone.
//!   - [`MAX_DELEGATE_DEPTH`]: the maximum recursion depth.
//!     Default is 2 (parent → child → grandchild is allowed,
//!     great-grandchild is rejected). The agent loop already
//!     enforces this via `delegate_depth` and
//!     `resolve_max_delegate_depth`; this module provides the
//!     authoritative constant.
//!   - [`filter_registry_for_subagent`]: a helper that takes
//!     a parent `ToolRegistry` and returns a child registry
//!     with the blocked tools removed.
//!   - [`contains_blocked_tool`]: returns the first blocked
//!     tool name found in a list, or `None` if the list is
//!     clean. Used to short-circuit `delegate_task` with a
//!     `Deny` verdict at the pre-dispatch hook.
//!
//! ## Why a deny-list, not an allow-list?
//!
//! The upstream project uses a deny-list for the same reason
//! we do: an allow-list would force the parent to enumerate
//! every safe tool, and new tools added by plugins would
//! automatically be blocked. A deny-list lets the
//! orchestrator pass through the parent's full registry
//! minus the known-dangerous subset.

use hermix_core::ToolSchema;

/// Tools that a sub-agent spawned by `delegate_task` is NEVER
/// permitted to call. The list is intentionally small and
/// hand-curated — these are the categories of tools that
/// cross the safety boundary between "the child can run a
/// bounded task" and "the child can take over the parent
/// session or escape into the wild".
///
/// ## Rationale per entry
///
/// - `delegate_task` — recursion guard. Without this, a
///   misbehaving child could chain `delegate_task` calls
///   infinitely. The depth cap covers accidental recursion;
///   the explicit block covers the case where a child is
///   configured with `max_depth = 0` (so the LLM might
///   believe the constraint is "no grandchildren" and try
///   to chain).
/// - `clarify` — interaction guard. A child must NEVER ask
///   the user a question. The parent's user-interaction
///   permission was granted to the parent, not delegated
///   to the child. If the child needs clarification, it
///   must surface that need in its final text and let the
///   parent decide.
/// - `memory` — write guard. The parent's `memory` tool
///   mutates the long-term memory store. Allowing a child
///   to write would mean the child can corrupt the
///   parent's persistent state. The child CAN read memory
///   through the `MemoryLayer`/`MemoryRouter` recall path,
///   it just can't write through the `memory` tool.
/// - `send_message` — outbound-IO guard. A child must NEVER
///   post messages to Telegram/Discord/Slack/etc. The
///   parent's `PermissionLevel` did not authorise the
///   child to talk on the user's behalf to third parties.
/// - `cronjob` — scheduling guard. A child must NEVER
///   schedule new jobs. The child runs to completion; it
///   does not get to leave background timers behind.
/// - `delegate_task` is duplicated here as a string for
///   explicit clarity in error messages. The deny-list is
///   a const slice of `&'static str` so it's safe to embed
///   in error messages without allocation.
pub const BLOCKED_TOOLS_FOR_SUBAGENT: &[&str] = &[
    "delegate_task",
    "clarify",
    "memory",
    "send_message",
    "cronjob",
];

/// Maximum delegate depth. The parent is depth 0, a direct
/// child is depth 1, a grandchild is depth 2. Depth 3
/// (great-grandchild) is rejected.
pub const MAX_DELEGATE_DEPTH: u32 = 2;

/// Return the first blocked tool name in `tool_names`, or
/// `None` if all of them are allowed. Used at pre-dispatch
/// to short-circuit `delegate_task` requests that try to
/// hand a child a blocked toolset.
pub fn contains_blocked_tool<S: AsRef<str>>(tool_names: &[S]) -> Option<String> {
    for name in tool_names {
        let n = name.as_ref();
        if BLOCKED_TOOLS_FOR_SUBAGENT.iter().any(|b| *b == n) {
            return Some(n.to_string());
        }
    }
    None
}

/// Return all blocked tools present in `tool_names`.
/// Convenience for reporting every violation at once rather
/// than failing on the first.
pub fn blocked_tools_in<S: AsRef<str>>(tool_names: &[S]) -> Vec<String> {
    tool_names
        .iter()
        .map(|n| n.as_ref().to_string())
        .filter(|n| BLOCKED_TOOLS_FOR_SUBAGENT.iter().any(|b| *b == n.as_str()))
        .collect()
}

/// Filter a list of `ToolSchema` to remove every entry whose
/// name is in [`BLOCKED_TOOLS_FOR_SUBAGENT`]. Returns a
/// fresh `Vec<ToolSchema>`.
pub fn filter_schemas_for_subagent(schemas: &[ToolSchema]) -> Vec<ToolSchema> {
    schemas
        .iter()
        .filter(|s| !is_blocked(&s.name))
        .cloned()
        .collect()
}

fn is_blocked(name: &str) -> bool {
    BLOCKED_TOOLS_FOR_SUBAGENT.iter().any(|b| *b == name)
}

/// Build a human-readable explanation of why a sub-agent
/// was rejected. Used by the pre-dispatch hook and by the
/// `sub_agent_orchestrator` when it builds the error
/// envelope.
pub fn block_reason(tool_name: &str) -> String {
    format!(
        "tool '{}' is in the sub-agent blocklist ({:?}); \
         a delegate_task child cannot call this tool. \
         See hermix_agent::sub_agent_safety::BLOCKED_TOOLS_FOR_SUBAGENT \
         for the full list and rationale.",
        tool_name, BLOCKED_TOOLS_FOR_SUBAGENT
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use hermix_core::{tool_schema, JsonSchema, ToolSchema};

    fn dummy_schema(name: &str) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("x".to_string(), serde_json::json!({"type": "integer"}));
        tool_schema(name, "test", JsonSchema::object(props, vec![]))
    }

    #[test]
    fn blocklist_includes_the_five_dangerous_tools() {
        for required in &[
            "delegate_task",
            "clarify",
            "memory",
            "send_message",
            "cronjob",
        ] {
            assert!(
                BLOCKED_TOOLS_FOR_SUBAGENT.contains(required),
                "blocklist missing {required}"
            );
        }
    }

    #[test]
    fn max_depth_is_two() {
        assert_eq!(MAX_DELEGATE_DEPTH, 2);
    }

    #[test]
    fn contains_blocked_tool_finds_blocked() {
        let names = vec!["read_file", "terminal", "memory"];
        assert_eq!(
            contains_blocked_tool(&names).as_deref(),
            Some("memory")
        );
    }

    #[test]
    fn contains_blocked_tool_returns_none_when_clean() {
        let names = vec!["read_file", "terminal", "glob"];
        assert!(contains_blocked_tool(&names).is_none());
    }

    #[test]
    fn contains_blocked_tool_empty_input() {
        let names: Vec<String> = vec![];
        assert!(contains_blocked_tool(&names).is_none());
    }

    #[test]
    fn blocked_tools_in_returns_all_violations() {
        let names = vec!["read_file", "delegate_task", "memory", "send_message"];
        let blocked = blocked_tools_in(&names);
        assert_eq!(blocked, vec!["delegate_task", "memory", "send_message"]);
    }

    #[test]
    fn filter_schemas_strips_blocked() {
        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("memory"),
            dummy_schema("terminal"),
            dummy_schema("clarify"),
            dummy_schema("glob"),
        ];
        let filtered = filter_schemas_for_subagent(&schemas);
        let names: Vec<String> = filtered.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"glob".to_string()));
        assert!(!names.contains(&"memory".to_string()));
        assert!(!names.contains(&"clarify".to_string()));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn filter_schemas_empty_input() {
        let empty: Vec<ToolSchema> = vec![];
        let filtered = filter_schemas_for_subagent(&empty);
        assert!(filtered.is_empty());
    }

    #[test]
    fn block_reason_mentions_tool_name() {
        let reason = block_reason("memory");
        assert!(reason.contains("memory"));
        assert!(reason.contains("sub-agent blocklist"));
        assert!(reason.contains("delegate_task"));
    }

    #[test]
    fn is_blocked_correctly_matches_case_sensitive() {
        // Tools are case-sensitive: "Memory" is not the
        // same as "memory". This matters because the
        // tool_registry is keyed by the exact name the
        // handler registered.
        assert!(is_blocked("memory"));
        assert!(!is_blocked("Memory"));
        assert!(!is_blocked("MEMORY"));
    }
}
