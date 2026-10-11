//! The "essential toolset" floor — toolset groups that must always be
//! enabled for the LLM to do useful (and safe) work, even if the user's
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
//! ## Two tiers (the floor does NOT override every user choice)
//!
//! The floor is split into a **hard** tier and a **soft** tier so that
//! genuinely optional toolsets respect the user's explicit disable
//! instead of being silently force-enabled on every startup (which
//! previously produced the "essential toolset floor overrode
//! whitelist" warning for toolsets like `document`).
//!
//! - [`HARD_ESSENTIAL_TOOLSETS`] — always re-asserted. These are the
//!   capabilities without which the product is unusable or unsafe:
//!   - `terminal` — run a shell command. Without it, the LLM cannot
//!     `ls`, `cat`, `git status`, `npm install`, etc.
//!   - `file` — read, write, patch, search files. Without it, the
//!     LLM cannot read its own source code, edit code, or list
//!     directories (via the `list_files` alias).
//!   - `code_execution` — run Python in a sandboxed subprocess.
//!     Without it, the LLM cannot compute, transform data, or run
//!     any non-trivial analysis.
//!   - `web` — search the web and extract page content. Without it,
//!     the LLM is offline-only.
//!   - `memory` — read/write long-term memory. Without it, every
//!     session is amnesiac.
//!   - `pdf` — read PDF documents. The product explicitly markets
//!     PDF summarization as a use case.
//!   - `core` — the `list_tools` introspection meta-tool, which is
//!     the LLM's escape hatch when it gets an "Unknown tool" error.
//!   - `custom_app` — create, package, and run user-facing desktop
//!     applications (the headline "build me an app" feature). Without
//!     it the LLM silently falls back to writing app files by hand
//!     into arbitrary paths (`release/apps/`, project root, …) which
//!     the custom-app UI can never load — exactly the "目录不对 /
//!     应用加载不到" failure mode.
//!   - `security` / `security_guidance` — the runtime safety
//!     guardrails. `url_safety` is tagged `security`;
//!     `security_guidance` is the guard's policy-file reader. Without
//!     these a hand-edited list can drop the LLM's ability to consult
//!     or update the safety policy mid-session, silently weakening
//!     the dangerous-command gate.
//!
//! - [`SOFT_ESSENTIAL_TOOLSETS`] — re-asserted **only** when the
//!   saved whitelist is in its default (`["*"]`) state. These are
//!   genuinely optional capabilities (`document`, `office`,
//!   `browser`, `cua`, `storage`): when the user has explicitly
//!   curated their toolset list and left one of them out, that is a
//!   deliberate disable and the floor must respect it. They are kept
//!   on the floor purely so a *fresh* install (wildcard) still sees a
//!   fully-capable agent out of the box.
//!
//! ## How to add or remove a toolset
//!
//! Edit [`HARD_ESSENTIAL_TOOLSETS`] / [`SOFT_ESSENTIAL_TOOLSETS`].
//! The startup reconciliation in `hermix-server/src/dashboard/skills.rs`
//! and the post-pass at the bottom of `register_builtin_tools` will
//! pick the change up automatically. **Add a unit test** that pins
//! the exact list — the tests act as a forcing function for "did I
//! really mean to add this to the floor?".

/// Hard essential toolsets — always re-enabled by the floor, even if
/// the user's `enabled_toolsets.json` would otherwise turn them off.
/// See the module docs for the full rationale.
pub const HARD_ESSENTIAL_TOOLSETS: &[&str] = &[
    "terminal",
    "file",
    "code_execution",
    "web",
    "memory",
    "pdf",
    "core",
    "security",
    "security_guidance",
    "custom_app",
    // workbench：与 custom_app 同级的头牌产品线（宿主内运行、对话迭代）。
    // SKILL 经 §13 层 3 每轮钉入工作台会话，工具必须可见——缺了它 agent
    // 必然臆想工具名。显式 curated 白名单（enabled_toolsets.json）是旧
    // 版本清单，新 toolset 永远不在里面，floor 是唯一可靠通道。
    "workbench",
    // vault：同一个坑的第二例（2026-10-11）。凭据保险库 2026-10-09 上线，
    // 但 `enabled_toolsets.json` 是更早那次「用户在设置里点过一次」时写下的
    // 快照，里面不可能有 vault → 启动时白名单后置过滤把整个 toolset 关掉，
    // `vault_list` / `vault_get` 对 LLM 完全不可见。用户侧的现象是
    // 「用远程对话问凭据，助手说它不知道有保险库这回事」——而前端面板照常
    // 可用（面板走 dashboard/vault_api 自己 open，与工具注册是两条路）。
    // 全程没有任何报错：工具注册成功、只是随后被静默关掉。
    // 没有做「新 toolset 默认可见」的通用迁移是刻意的：白名单是安全闸，
    // 让缺席项自动转正等于废掉它；这里的取舍是——保险库是产品头牌能力，
    // 且本地会话按设计天然可信（见 tools/vault.rs 顶部信任边界），故进硬地板。
    "vault",
];

/// Soft essential toolsets — re-enabled by the floor ONLY when the
/// user's saved whitelist is in its default (`["*"]`) state. When the
/// user has explicitly curated their toolset list and omitted one of
/// these, that omission is a deliberate disable and the floor respects
/// it. This is the fix for the "essential toolset floor overrode
/// whitelist" warning firing on startup for toolsets like `document`.
pub const SOFT_ESSENTIAL_TOOLSETS: &[&str] = &[
    "document",
    "office",
    "browser",
    "cua",
    "storage",
];

/// The combined floor, kept for callers and tests that pin the exact
/// total set of essential toolsets. Equivalent to the concatenation of
/// [`HARD_ESSENTIAL_TOOLSETS`] and [`SOFT_ESSENTIAL_TOOLSETS`].
pub const ESSENTIAL_TOOLSETS: &[&str] = &[
    // hard
    "terminal",
    "file",
    "code_execution",
    "web",
    "memory",
    "pdf",
    "core",
    "security",
    "security_guidance",
    "custom_app",
    "workbench",
    "vault",
    // soft
    "document",
    "office",
    "browser",
    "cua",
    "storage",
];

/// Re-enable the essential toolset floor on a [`ToolRegistry`].
///
/// `soft_floor_active` controls whether the [`SOFT_ESSENTIAL_TOOLSETS`]
/// tier is also re-asserted:
/// - pass `true` only when the user's saved whitelist is in its default
///   (`["*"]`) state — i.e. the user has not explicitly curated which
///   toolsets they want. A fresh install therefore boots with a fully
///   capable agent.
/// - pass `false` when an explicit list exists (the user is curating),
///   so a soft toolset they left out stays disabled. This is what
///   stops the floor from overriding the user's explicit disable of,
///   e.g., `document`.
///
/// The [`HARD_ESSENTIAL_TOOLSETS`] tier is always re-asserted.
///
/// Idempotent; safe to call multiple times. Returns the number of
/// (toolset, count) flips actually performed, useful for tracing.
pub fn ensure_essentials_enabled(registry: &crate::ToolRegistry, soft_floor_active: bool) -> usize {
    let mut total = 0usize;
    for ts in HARD_ESSENTIAL_TOOLSETS {
        total += registry.set_toolset_enabled(ts, true);
    }
    if soft_floor_active {
        for ts in SOFT_ESSENTIAL_TOOLSETS {
            total += registry.set_toolset_enabled(ts, true);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_essential_list_is_pinned() {
        // Pin the exact contents. Adding/removing a hard toolset is a
        // deliberate decision; this test forces a code review by failing
        // if the list changes silently.
        assert_eq!(
            HARD_ESSENTIAL_TOOLSETS,
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
                "custom_app",
                "workbench",
                "vault",
            ],
            "HARD_ESSENTIAL_TOOLSETS changed — update the floor and any UI copy."
        );
    }

    #[test]
    fn soft_essential_list_is_pinned() {
        // Soft toolsets must stay genuinely optional; this test pins the
        // list so a future "tidy-up" cannot silently promote one to hard
        // and re-introduce the startup override warning.
        assert_eq!(
            SOFT_ESSENTIAL_TOOLSETS,
            &["document", "office", "browser", "cua", "storage"],
            "SOFT_ESSENTIAL_TOOLSETS changed — soft toolsets must remain optional."
        );
    }

    #[test]
    fn essential_list_is_union_of_tiers() {
        let mut combined: Vec<&str> = Vec::new();
        combined.extend_from_slice(HARD_ESSENTIAL_TOOLSETS);
        combined.extend_from_slice(SOFT_ESSENTIAL_TOOLSETS);
        assert_eq!(
            ESSENTIAL_TOOLSETS,
            combined.as_slice(),
            "ESSENTIAL_TOOLSETS must stay the concatenation of the two tiers"
        );
    }

    #[test]
    fn essential_lists_have_no_duplicates() {
        // `set_toolset_enabled` is idempotent so duplicates would not
        // cause a bug, but they would suggest sloppy editing.
        let mut seen: Vec<&str> = Vec::new();
        for ts in ESSENTIAL_TOOLSETS {
            if seen.contains(ts) {
                panic!("ESSENTIAL_TOOLSETS contains duplicate: {ts}");
            }
            seen.push(ts);
        }
    }

    #[test]
    fn hard_essential_contains_critical_toolsets() {
        // The previous failure mode was the LLM being told it had no
        // `terminal` tool. Hard-pin the presence of the critical
        // capabilities so a future "tidy-up" cannot silently
        // re-introduce the regression.
        assert!(
            HARD_ESSENTIAL_TOOLSETS.contains(&"terminal"),
            "terminal must remain in the hard essential floor"
        );
        assert!(
            HARD_ESSENTIAL_TOOLSETS.contains(&"code_execution"),
            "code_execution (Python sandbox) must remain in the hard essential floor"
        );
        assert!(
            HARD_ESSENTIAL_TOOLSETS.contains(&"core"),
            "core (list_tools introspection) must remain in the hard essential floor"
        );
        assert!(
            HARD_ESSENTIAL_TOOLSETS.contains(&"security_guidance"),
            "security_guidance (policy reader) must remain in the hard essential floor"
        );
        // 凭据保险库：2026-10-11 从白名单里漏掉过一次，LLM 因此答"不知道有
        // 凭据保险库"。钉住它，避免下一次"整理"时又被移出硬地板。
        assert!(
            HARD_ESSENTIAL_TOOLSETS.contains(&"vault"),
            "vault (credential vault read tools) must remain in the hard essential floor"
        );
    }
}
