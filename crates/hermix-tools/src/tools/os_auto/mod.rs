//! Native OS automation — two independent capability lines, both
//! in-house (no Python / Node / .NET / external runtime on the user's
//! machine). Design + pitfall inventory:
//! `docs/notes/implemented/feature/2026-10-13-native-os-automation.md`.
//!
//! - [`browser`] drives the **system Edge** over CDP (chromiumoxide).
//!   The process is contained by a Job Object with
//!   `KILL_ON_JOB_CLOSE`, so hermix crashing cannot orphan a browser.
//! - [`desktop`] drives **UI Automation** COM through one dedicated
//!   MTA worker thread. COM apartments are thread-bound; calling UIA
//!   from a tokio worker is what hung a whole run in the deleted
//!   `hermix-cua` (commit `c6442032`), so every call is funnelled
//!   through that thread with a hard timeout.
//!
//! `desktop` is **opt-in**: it is registered, but stays disabled until the
//! operator flips it on in Settings → Toolsets. See [`OPT_IN_TOOLSETS`]
//! and [`apply_opt_in_defaults`]. `browser` left the opt-in list when the
//! virtual-window P0 landed (2026-10-14) — it is on the essential hard
//! floor, see `crate::essential::HARD_ESSENTIAL_TOOLSETS`.

pub mod browser;
pub mod desktop;

use std::collections::HashSet;

use crate::ToolRegistry;

/// Truncate on a **character** boundary.
///
/// Byte truncation (`&s[..n]`) panics on a multi-byte boundary, and every
/// string either line hands back is user-visible text that is routinely
/// Chinese — window titles, element names, page content. This is the single
/// home for that rule so no output cap in either line can get it wrong.
///
/// The registry's own `max_result_size_chars` does **not** go through here:
/// it clips on bytes. The OS-automation tools therefore register with `None`
/// and cap the result themselves.
pub fn truncate_chars(s: &str, max: usize) -> String {
    // 单一实现：`hermix_core::text_utils` 的版本按 char 边界安全切分；
    // 这里此前是第二份 `chars().take()`。
    hermix_core::text_utils::truncate_chars(s, max).to_string()
}

/// Toolset tag for the browser line.
pub const BROWSER_TOOLSET: &str = "browser";
/// Toolset tag for the desktop line.
pub const DESKTOP_TOOLSET: &str = "desktop";

/// Toolsets that ship **off**, even on a fresh install whose
/// `enabled_toolsets.json` is the `["*"]` wildcard.
///
/// Rationale: a wildcard means "everything we know about", and the
/// operator has never seen these tools in the UI. Driving the user's
/// real desktop (keyboard injection into whatever window is focused)
/// is the most invasive thing this agent can do; it should not switch
/// on because of a default. The Settings → Toolsets tab already
/// renders any registered toolset with a toggle, so opting in is one
/// click.
///
/// `browser` used to be on this list for the same reason ("drives the
/// user's real browser"), but the virtual-window P0 (2026-10-14)
/// removed that concern: the automation browser is always headless
/// against a throwaway profile, inputs are synthetic CDP events, and
/// the picture only ever shows in the app's own sub-window. It moved
/// to the essential hard floor — see `essential::HARD_ESSENTIAL_TOOLSETS`.
pub const OPT_IN_TOOLSETS: &[&str] = &[DESKTOP_TOOLSET];

/// Register the OS-automation lines. Individual handlers own their own risk
/// levels; this only wires the toolsets into the registry.
///
/// The headless `browser` line was retired on 2026-10-21: the viewer's MCP
/// server now owns browser automation (`viewer__browser_*`), including a
/// `headed` switch (`browser_open(headed=false)` = headless, same throwaway
/// profiles and CDP semantics this module had). Running both registrations
/// gave the model two same-named tools with different parameter shapes —
/// `browser_click(ref)` vs `viewer__browser_click(x, y)` — which is the
/// parameter-guessing failure mode we keep getting burned by. The module code
/// stays: the viewer's browser module documents the same three pitfalls and
/// can borrow from here, and re-enabling is one line.
pub fn register(registry: &ToolRegistry) {
    desktop::register(registry);
}

/// Disable every [`OPT_IN_TOOLSETS`] entry the operator has not named
/// explicitly in `enabled`.
///
/// Called from **two** places, and they must agree or a restart flips
/// the toolsets back on:
///
/// 1. `register_builtin_tools` — after the whitelist post-pass and the
///    essential floor, both of which only ever *enable* toolsets.
/// 2. `hermix-server`'s `apply_persisted_toolset_state` — after its
///    wildcard-aware reconciliation loop.
///
/// `enabled` is the operator's whitelist set, which may contain the
/// wildcard `"*"` / `"all"`. A wildcard is **not** an explicit opt-in:
/// only a literal toolset name counts. `None` means "no whitelist was
/// supplied" (tests, CLI) and is treated as "not opted in".
///
/// Returns how many toolsets were flipped off.
pub fn apply_opt_in_defaults(registry: &ToolRegistry, enabled: Option<&HashSet<String>>) -> usize {
    let toolsets = registry.list_toolsets();
    let registered: HashSet<&str> = toolsets.iter().map(|s| s.as_str()).collect();
    let mut flipped = 0usize;
    for toolset in OPT_IN_TOOLSETS {
        if !registered.contains(toolset) {
            continue;
        }
        let explicit = enabled.is_some_and(|set| set.contains(*toolset));
        if explicit {
            continue;
        }
        let entries = registry.set_toolset_enabled(*toolset, false);
        if entries > 0 {
            flipped += 1;
            tracing::info!(
                toolset,
                entries,
                "opt-in toolset disabled until the operator enables it in Settings"
            );
        }
    }
    flipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_is_not_an_opt_in() {
        // The whole point of OPT_IN_TOOLSETS: `["*"]` means "everything
        // the agent already knew about", not "opt me into taking over
        // the user's mouse and keyboard".
        let set: HashSet<String> = ["*".to_string(), "all".to_string()].into_iter().collect();
        for toolset in OPT_IN_TOOLSETS {
            assert!(
                !set.contains(*toolset),
                "wildcard set must not be read as an explicit opt-in for {toolset}"
            );
        }
    }

    #[test]
    fn opt_in_toolsets_are_the_desktop_line_only() {
        // `browser` left the opt-in list in 2026-10-16 (virtual-window P0),
        // then left the essential floor entirely on 2026-10-21: the viewer's
        // MCP server owns browser automation now, and this module no longer
        // registers it. Only `desktop` (real keyboard injection into whatever
        // window is focused) still ships off.
        assert_eq!(OPT_IN_TOOLSETS, &[DESKTOP_TOOLSET]);
        assert!(!OPT_IN_TOOLSETS.contains(&BROWSER_TOOLSET));
        // A wildcard must never be mistaken for a real toolset name.
        assert!(!OPT_IN_TOOLSETS.contains(&"*"));
        assert!(!OPT_IN_TOOLSETS.contains(&"all"));
    }

    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        // 3 bytes per char: a byte-boundary clip at 8 would panic outright and
        // a clip at 9 would return invalid UTF-8.
        let capped = truncate_chars(&"字".repeat(100), 8);
        assert_eq!(capped.chars().count(), 8);
        assert_eq!(truncate_chars("short", 8), "short");
    }
}