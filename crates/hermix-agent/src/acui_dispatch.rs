//! ACUI directive dispatch helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No
//! behavior change** — pure relocation.
//!
//! Items living here:
//! - `emit_acui` — fire an `AcuiDirective` through the
//!   `AgentCallbacks::on_acui_directive` channel.
//! - `emit_acui_show` / `emit_acui_update` / `emit_acui_hide` —
//!   convenience wrappers around the three variant constructors.
//! - `default_tool_router` — `KeywordRouter + AlwaysInclude`
//!   layered routing. This is the canonical router the agent loop
//!   wires in `AgentLoop::new` unless the caller overrides it via
//!   `AgentLoop::with_tool_router`.

use std::sync::Arc;

use hermix_core::{AcuiDirective, AcuiHide, AcuiShow, AcuiUpdate};
use hermix_intelligence::{AlwaysInclude, KeywordRouter, ToolSchemaRouter};

use crate::agent_helpers::AgentCallbacks;

/// Fire an `AcuiDirective` through `AgentCallbacks::on_acui_directive`.
/// This function never blocks and never returns an error — the ACUI
/// pipeline is best-effort. A failure in the callback is logged and
/// swallowed (the agent keeps running).
pub fn emit_acui(callbacks: &AgentCallbacks, directive: AcuiDirective) {
    let Some(cb) = callbacks.on_acui_directive.as_ref() else {
        return;
    };
    match serde_json::to_string(&directive) {
        Ok(payload) => cb(&payload),
        Err(e) => {
            tracing::warn!(
                target: "hermix.agent",
                error = %e,
                "failed to serialize AcuiDirective, dropping emit"
            );
        }
    }
}

/// Convenience: emit a `Show` directive.
pub fn emit_acui_show(
    callbacks: &AgentCallbacks,
    id: impl Into<String>,
    kind: impl Into<String>,
    props: serde_json::Value,
) {
    emit_acui(callbacks, AcuiDirective::Show(AcuiShow::new(id, kind, props)));
}

/// Convenience: emit an `Update` directive.
pub fn emit_acui_update(callbacks: &AgentCallbacks, id: impl Into<String>, patch: serde_json::Value) {
    emit_acui(callbacks, AcuiDirective::Update(AcuiUpdate::new(id, patch)));
}

/// Convenience: emit a `Hide` directive.
pub fn emit_acui_hide(callbacks: &AgentCallbacks, id: impl Into<String>) {
    emit_acui(callbacks, AcuiDirective::Hide(AcuiHide::new(id)));
}

/// Build a default router (KeywordRouter + AlwaysInclude).
/// Construct the canonical tool schema router (`KeywordRouter +
/// AlwaysInclude`). The whitelist is the **union** of:
/// 1. **System-level always-forward tools** (hardcoded): tool families
/// the LLM must always see because `KeywordRouter`'s scoring on
/// CJK-heavy or generic-language queries can score them ≈ 0. Without
/// always-forwarding, the agent falls back to `terminal cd/ls` to
/// "find" a file the user already gave a path for. This
/// list is intentionally not user-configurable — it's a safety /
/// capability gate, not a routing preference.
/// 2. **User-configurable `always_include` whitelist**: caller-supplied
/// via `AgentLoop::always_include` (e.g. user adds `"delegate_task"`
/// via `.with_always_include()`). Plumbed in previously
/// the field existed but the router ignored it.
pub fn default_tool_router(user_always_include: &[String]) -> Arc<dyn ToolSchemaRouter> {
    // v2: added delegate_task, read_image, move_file, list_tools to system whitelist
    let mut whitelist: Vec<String> = vec![
        // System-level always-forward (see doc above).
        "read_file".to_string(),
        "write_file".to_string(),
        "terminal".to_string(),
        "memory".to_string(),
        // daily_memory must always be visible like `memory`: the
        // operating-principles block mandates goal/progress/conclusion
        // notes, but the router can score it ≈ 0 on CJK queries and
        // truncate it out of the schema set, after which the day's
        // daily-memory row stays an empty skeleton.
        "daily_memory".to_string(),
        // read_pdf and list_files must be always-forwarded so the
        // LLM does not fall back to "guess the path via terminal"
        // when the user supplies an explicit file path (e.g.
        // "根据 G:\…\112号…通知.pdf 起草通知").  Previously the
        // KeywordRouter scored read_pdf ≈ 0 on CJK-heavy queries,
        // causing the agent to waste turns on terminal cd/ls
        // instead of calling the tool directly.
        "read_pdf".to_string(),
        "list_files".to_string(),
        // delegate_task and read_image must be always-forwarded
        // so the LLM can spawn sub-agents for image/binary file
        // analysis. Without this, the KeywordRouter scores them ≈ 0
        // on CJK-heavy queries (e.g. "图片按内容重命名"), and the
        // LLM falls back to read_file on JPEGs (which fails with
        // UTF-8 errors) or terminal OCR (which has no tesseract).
        // This caused a 13-turn self-spin loop in real testing.
        "delegate_task".to_string(),
        "read_image".to_string(),
        // move_file must be always-forwarded so the LLM can rename
        // files without resorting to terminal `mv`/`Rename-Item`
        // (which has quoting issues on Windows paths with CJK).
        "move_file".to_string(),
        // list_tools must be always-forwarded so the LLM can see the whole
        // roster in **one** call (names + one-line descriptions) instead of
        // paying the two-hop bridge (`tool_search` → `tool_describe`). It
        // scores ≈ 0 under KeywordRouter for CJK-heavy queries ("把图片按内容
        // 重命名" contains none of its name tokens), so without this entry the
        // router truncates it and the model falls back to guessing tool names.
        //
        // History — this comment used to claim `tool_search` "doesn't exist
        // as a callable tool". That was true when the entry was added
        // (2026-07-27) and stopped being true on 2026-10-05, when `f4820122`
        // ("接通 10 个死工具") registered the bridge. `list_tools` keeps its
        // slot because it is still the cheapest full-roster view, **not**
        // because the bridge is missing. Do not cite the bridge's absence as
        // the reason again — it exists (`tool_search.rs::register`, called
        // from `tools/mod.rs`).
        "list_tools".to_string(),
        // skill_view / skills_list must be always-forwarded — the whole skill
        // system is **progressive disclosure**: the prompt carries the
        // `<available_skills>` index, the body is loaded on demand. Two things
        // break when routing drops them:
        //   1. `skills_system_prompt` only renders the index when one of
        //      `skills_list` / `skill_view` / `skill_manage` is in the active
        //      set — no tool, no index, skills become invisible for the turn;
        //   2. auto-matched large skills inject a **pointer** (`skill_view`),
        //      which is a dead end if the tool isn't there.
        // Both schemas are tiny; the failure they prevent (model never learns
        // the skill exists) is silent.
        "skill_view".to_string(),
        "skills_list".to_string(),
        // workbench_manager must be always-forwarded so workbench-bound
        // sessions (skill pinned every turn, MODULE_DESIGN §13 layer 3)
        // can actually call it. KeywordRouter scores it ≈ 0 on
        // CJK-heavy queries (its name tokens never appear in Chinese
        // requests like "给工作台加个图表"), so top-N truncation drops
        // it — same failure shape that read_pdf / delegate_task were
        // whitelisted for.
        "workbench_manager".to_string(),
        // The deferred-tool bridge is atomic: `tool_search` → `tool_describe`
        // → `tool_call` is one procedure, so a turn that keeps two of them and
        // drops the third is strictly worse than keeping none — the model sees
        // the workflow advertised (the system prompt names all three) but one
        // step is unreachable. Without this entry the router's keyword scoring
        // decides: a query containing "search"/"call" keeps those two by name
        // while `tool_describe` scores ~0 and is truncated by `max_tools` — the
        // exact shape of the 2026-10-21 "four wasted turns" report.
        "tool_search".to_string(),
        "tool_describe".to_string(),
        "tool_call".to_string(),
    ];
    // Dedupe + append user whitelist. De-dup with system-level so the
    // user-supplied names don't appear twice in `AlwaysInclude::route()`
    // log output (the wrapper dedupes internally too, but the trace
    // gets noisy).
    for name in user_always_include {
        if !whitelist.contains(name) {
            whitelist.push(name.clone());
        }
    }
    let kw = Arc::new(KeywordRouter::new());
    let always = Arc::new(AlwaysInclude::new(kw, whitelist));
    // PriorityBoost: L1 Domain Mode integration. `TurnContext::priority_tools`
    // flows from `DomainPreset::priority_tools` via `AgentLoopConfig::route_tools`;
    // this wrapper re-ranks those tools (+5.0) without force-including them.
    // Empty priority list (Chat domain / no preset tools) → pass-through.
    Arc::new(hermix_intelligence::PriorityBoost::with_default_boost(always))
}

#[cfg(test)]
mod tests {
    //! regression coverage for the `always_include` field
    //! plumbing. Prior to this fix the field on `AgentLoop` existed
    //! but `default_tool_router()` ignored it, so user-configured
    //! always-include names were silently dropped on the floor.
    //!
    //! We can't construct a full `AgentLoop` for this test (it owns
    //! a `MemoryRouter`, callbacks, focus stack, etc.), but the router
    //! itself is a pure data structure: `default_tool_router(list)`
    //! returns `Arc<dyn ToolSchemaRouter>`. Verifying that the user's
    //! list reaches the `AlwaysInclude` wrapper is a direct read of
    //! the new wiring contract.

    use super::*;
    use hermix_core::tool_schema::{JsonSchema, ToolSchema};
    // `ToolSchemaRouter` is referenced only via `Arc<dyn ToolSchemaRouter>` in the
    // module-level doc comment; trait import is needed for the `dyn` pointer type
    // the comment references. Suppress the unused-import lint locally.
    #[allow(unused_imports)]
    use hermix_intelligence::{RoutedSchema, ToolSchemaRouter, TurnContext};

    fn tool(name: &str, desc: &str) -> ToolSchema {
        ToolSchema::new(name, desc, JsonSchema::new("object"))
    }

    /// `default_tool_router` must include every user-passed entry
    /// even when the inner `KeywordRouter` would have truncated it
    /// out. We probe with a candidate list large enough that the
    /// inner router's `max_tools = 5` truncation by name drops the
    /// user-supplied entry — then `AlwaysInclude` re-adds it with
    /// `force_included = true`.
    /// Pre-the `AgentLoop::always_include` field was
    /// never wired into `default_tool_router`, so this assertion
    /// would have failed for any non-system default entry.
    #[test]
    fn default_tool_router_carries_user_always_include_through_to_wrapper() {
        let user_extra = vec!["zzz_user_tool".to_string()];
        let router = default_tool_router(&user_extra);

        // 9 candidates, names sorted alphabetically so the
        // lexicographically last (`zzz_user_tool`) is dropped by
        // `KeywordRouter`'s `scored.truncate(ctx.max_tools)` (default
        // max_tools = 5 → top 5 by name survive). 9 - 5 = 4 drops,
        // confirm zzz_user_tool is among them.
        let candidates = vec![
            tool("aaa_one", "z"),
            tool("bbb_two", "z"),
            tool("ccc_three", "z"),
            tool("ddd_four", "z"),
            tool("eee_five", "z"),
            tool("fff_six", "z"),
            tool("ggg_seven", "z"),
            tool("hhh_eight", "z"),
            tool("zzz_user_tool", "user-pushed"),
        ];
        let ctx = TurnContext::from_user_message("nothing keyword-relevant");
        let routed: Vec<RoutedSchema> = router.route(&ctx, candidates);

        // user-pushed tool must appear in routed output.
        let names: Vec<&str> =
            routed.iter().map(|r| r.schema.name.as_str()).collect();
        assert!(
            names.contains(&"zzz_user_tool"),
            "user-supplied always-include `zzz_user_tool` must be force-included by AlwaysInclude; routed = {names:?}"
        );
        let z = routed
            .iter()
            .find(|r| r.schema.name == "zzz_user_tool")
            .expect("zzz_user_tool entry present");
        assert!(
            z.force_included,
            "user always-include must be marked force_included=true by AlwaysInclude"
        );
    }

    /// User-supplied names already covered by the system-level list
    /// should appear exactly once in the whitelist (no dupe noise in
    /// AlwaysInclude iteration). The deduplication happens inside
    /// `default_tool_router` before handing off to `AlwaysInclude::new`.
    #[test]
    fn default_tool_router_dedupes_user_against_system_list() {
        // `read_file` is in the system-level hardcoded list. Passing
        // it again via `user_always_include` must not append a
        // duplicate.
        let user = vec!["read_file".to_string()];
        let router = default_tool_router(&user);

        let candidates = vec![tool("read_file", "read")];
        let ctx = TurnContext::from_user_message("anything");
        let routed = router.route(&ctx, candidates);
        let read_file_count = routed.iter().filter(|r| r.schema.name == "read_file").count();
        assert_eq!(
            read_file_count, 1,
            "read_file must appear exactly once even when listed by both system and user"
        );
    }
}
