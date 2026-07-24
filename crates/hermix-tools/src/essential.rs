//! The "essential toolset" floor — toolset groups that must always be
//! enabled for the LLM to do useful work, even if the user's
//! `enabled_toolsets.json` whitelist would otherwise exclude them.
//!
//! ## Why this exists
//!
//! The Settings UI persists an explicit whitelist of which
//! toolsets the LLM may see (`~/.hermix/enabled_toolsets.json`).
//! A new install defaults to `["*"]` (wildcard) so every toolset is
//! visible; once the user toggles a toolset off, the file becomes
//! an explicit list. **In practice, a hand-edited or partially
//! migrated list can end up containing only "obvious" toolsets
//! like `file` and `web`, omitting `terminal` / `code_execution` /
//! `memory` / `pdf` / `core`.** The LLM then sees only 14–20 tools
//! in its `tools` API field and answers "I don't have terminal /
//! execute_code / list_files", even though those tools are
//! registered and ready to run.
//!
//! The essential floor prevents this. After every reconciliation
//! pass (both at startup from `apply_persisted_toolset_state` and
//! at the bottom of `register_builtin_tools`), these toolset
//! groups are unconditionally re-enabled. The user can still see
//! them as "on" in the Settings UI and *additionally* turn them
//! off — but the next reconcile pass will re-enable them, which
//! is exactly the point: removing any of these by accident
//! degrades the LLM into a "no shell, no memory, no
//! introspection" mode that is useless for the desktop product.
//!
//! ## What "essential" means here
//!
//! The list is **deliberately short**. It covers the four
//! capabilities without which the LLM cannot do the things a
//! user opens the desktop app to do:
//!
//! - `terminal` — run a shell command. Without it, the LLM cannot
//!   `ls`, `cat`, `git status`, `npm install`, etc.
//! - `file` — read, write, patch, search files. Without it, the
//!   LLM cannot read its own source code, edit code, or list
//!   directories (via the `list_files` alias).
//! - `code_execution` — run Python in a sandboxed subprocess.
//!   Without it, the LLM cannot compute, transform data, or run
//!   any non-trivial analysis.
//! - `web` — search the web and extract page content. Without it,
//!   the LLM is offline-only.
//! - `memory` — read/write long-term memory. Without it, every
//!   session is amnesiac.
//! - `pdf` — read PDF documents. The product explicitly markets
//!   PDF summarization as a use case.
//! - `core` — the `list_tools` introspection meta-tool, which is
//!   the LLM's escape hatch when it gets an "Unknown tool" error.
//!
//! Everything else (`browser`, `tts`, `homeassistant`,
//! `image_gen`, etc.) is genuinely optional and respects the
//! user's whitelist.
//!
//! ## How to add or remove a toolset
//!
//! Edit the [`ESSENTIAL_TOOLSETS`] constant. The startup
//! reconciliation in `hermix-server/src/dashboard/skills.rs` and
//! the post-pass at the bottom of `register_builtin_tools` will
//! pick the change up automatically. **Add a unit test** in
//! `hermix_tools::essential::tests` that pins the exact list —
//! the test acts as a forcing function for "did I really mean to
//! add this to the floor?".

/// The essential toolset floor. The constant is `pub` so other
/// crates (`hermix-server`, `hermix-agent`) can reference the
/// canonical list without copy-pasting the strings.
/// /// P1.8 (2026-07-02) — alignment pass with the runtime registry:
/// - `"computer_use"` removed. There is no tool tagged
/// `"computer_use"`. The previous `register_one(name, name, ...)`
/// pattern in `tools::cua::register` put every CUA handler under
/// its own per-tool name, so the `set_toolset_enabled("computer_use", true)`
/// call was a silent no-op. CUA coverage is now provided by the
/// `"cua"` toolset entry (added in the same pass: see
/// `tools::cua::register`).
/// - `"security_guidance"` added. `url_safety` is tagged `"security"`
/// and re-asserted by the floor, but `security_guidance` (the
/// runtime guard's policy file reader) was its own toolset that the
/// floor missed. Without this entry a hand-edited
/// `enabled_toolsets.json` containing only `["file", "web"]` would
/// leave the LLM unable to read or update the security policy
/// mid-session.
pub const ESSENTIAL_TOOLSETS: &[&str] = &[
    "terminal",
    "file",
    "code_execution",
    "web",
    "memory",
    "pdf",
    "core",
    "security",
    "security_guidance",
    "office",
    "browser",
    "cua",
    "storage",
];

/// Re-enable the essential toolsets on a [`ToolRegistry`]. Idempotent;
/// safe to call multiple times. Returns the number of
/// (toolset, count) flips actually performed, useful for tracing.
/// /// The function does **not** disable anything the user explicitly
/// enabled in the whitelist — it only re-asserts the floor. If
/// the user toggled `terminal` off and the next reconciliation
/// pass turned it off, calling this re-enables it; if `terminal`
/// was already on, the call is a no-op.
pub fn ensure_essentials_enabled(registry: &crate::ToolRegistry) -> usize {
    let mut total = 0usize;
    for ts in ESSENTIAL_TOOLSETS {
        // `set_toolset_enabled` returns the number of entries
        // flipped. We sum across all essentials for a single
        // return value the caller can log.
        total += registry.set_toolset_enabled(ts, true);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essential_list_is_non_empty_and_well_known() {
        // Pin the exact contents. Adding/removing a toolset here
        // is a deliberate decision; this test forces a code
        // review by failing if the list changes silently.
        assert_eq!(
            ESSENTIAL_TOOLSETS,
            &[
                "terminal",
                "file",
                "code_execution",
                "web",
                "memory",
                "pdf",
                "core",
                "security",
                "security_guidance",
                "office",
                "browser",
                "cua",
                "storage",
            ],
            "ESSENTIAL_TOOLSETS changed — update the tests in \
             hermix_tools::essential and any UI copy that lists \
             the floor."
        );
    }

    #[test]
    fn essential_list_has_no_duplicates() {
        // `set_toolset_enabled` is idempotent so duplicates
        // would not cause a bug, but they would suggest sloppy
        // editing. Catch the smell.
        let mut seen: Vec<&str> = Vec::with_capacity(ESSENTIAL_TOOLSETS.len());
        for ts in ESSENTIAL_TOOLSETS {
            if seen.contains(ts) {
                panic!("ESSENTIAL_TOOLSETS contains duplicate: {ts}");
            }
            seen.push(ts);
        }
    }

    #[test]
    fn essential_list_contains_critical_toolset() {
        // The previous failure mode was the LLM being told it
        // had no `terminal` tool. Hard-pin the presence of
        // `terminal` so a future "tidy-up" of the list cannot
        // silently re-introduce the regression.
        assert!(
            ESSENTIAL_TOOLSETS.contains(&"terminal"),
            "terminal must remain in the essential floor"
        );
        assert!(
            ESSENTIAL_TOOLSETS.contains(&"code_execution"),
            "code_execution (Python sandbox) must remain in the essential floor"
        );
        assert!(
            ESSENTIAL_TOOLSETS.contains(&"core"),
            "core (list_tools introspection) must remain in the essential floor"
        );
    }
}
