//! Skills and toolset management endpoints.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::types::*;
use crate::{HttpError, HttpServerState};

/// GET /api/skills
pub async fn list_skills(State(state): State<HttpServerState>) -> Json<Vec<SkillInfo>> {
    let Some(store) = state.skill_store.as_ref() else {
        return Json(vec![]);
    };

    let metas = store.list().await.unwrap_or_default();
    Json(
        metas
            .into_iter()
            .map(|m| SkillInfo {
                name: m.name,
                description: m.description.unwrap_or_default(),
                category: m.category.unwrap_or_default(),
                enabled: true, // file-based skills are always enabled if present
            })
            .collect(),
    )
}

/// GET /api/skills/{name}
pub async fn get_skill(
    State(state): State<HttpServerState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SkillContentResponse>, HttpError> {
    let Some(store) = state.skill_store.as_ref() else {
        return Err(HttpError {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            message: "skill store not configured".to_string(),
        });
    };

    match store.load(&name).await {
        Ok(Some(skill)) => Ok(Json(SkillContentResponse {
            name: skill.name,
            content: skill.content,
            category: skill.category,
            description: skill.description,
        })),
        Ok(None) => Err(HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("skill '{}' not found", name),
        }),
        Err(e) => Err(HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        }),
    }
}

/// PUT /api/skills/toggle
pub async fn toggle_skill(
    State(state): State<HttpServerState>,
    Json(body): Json<SkillToggle>,
) -> Result<Json<OkResponse>, HttpError> {
    let Some(store) = state.skill_store.as_ref() else {
        return Err(HttpError {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            message: "skill store not configured".to_string(),
        });
    };

    if !body.enabled {
        // Disable: move skill directory to .disabled/ sibling
        store.disable(&body.name).await.map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;
    } else {
        // Enable: move skill back from .disabled/ sibling
        store.enable(&body.name).await.map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;
    }

    Ok(Json(OkResponse { ok: true }))
}

/// GET /api/tools/toolsets
/// Returns the **toolset categories** actually registered in the
/// in-process `ToolRegistry` (e.g. `web`, `terminal`, `file`,
/// `memory`, `browser`, ...), not individual tool definitions —
/// the previous implementation conflated the two and the UI
/// displayed one entry per tool. For each category we attach:
/// * `tools` — the list of tool names belonging to it
/// (filtered by `e.toolset == category`),
/// * `enabled` — the persisted + runtime current state (we read
/// the per-tool `is_enabled` flag and AND-reduce across the
/// toolset; if any tool in the group is disabled the whole
/// toolset is reported as disabled, since the LLM will not see
/// the disabled ones and we want the toggle to reflect that),
/// * `configured` — always `true` for built-in toolsets (the
/// "needs configuration" badge is reserved for plugin-style
/// optional adapters that the upstream has but we do not
/// surface here yet).
pub async fn list_toolsets(State(state): State<HttpServerState>) -> Json<Vec<ToolsetInfo>> {
    // Use `list_tools()` (not `get_definitions()`) so we see
    // *every* registered tool, including the ones whose
    // `is_enabled` is currently false — otherwise a freshly
    // disabled toolset would disappear from this list and the
    // UI could not toggle it back on.
    let all_tools = state.tool_registry.list_tools();

    // Group tool names by toolset tag, preserving the order
    // `ToolRegistry::list_toolsets()` returns (sorted, deduped)
    // so the UI's grid is stable across reloads.
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for t in &all_tools {
        grouped.entry(t.toolset.clone()).or_default().push(t.name.clone());
    }

    let mut out: Vec<ToolsetInfo> = Vec::with_capacity(grouped.len());
    for (ts_name, tools) in grouped {
        // A toolset is reported `enabled` only if every tool in
        // it has `is_enabled=true`. The Settings UI toggles
        // the whole group at once (via
        // `ToolRegistry::set_toolset_enabled`), so in practice
        // this is always uniform — but we read the live state
        // defensively in case a partial-disable path ever
        // lands.
        let total = tools.len();
        let enabled_count = all_tools
            .iter()
            .filter(|t| t.toolset == ts_name)
            .filter(|t| state.tool_registry.is_available(&t.name))
            .count();
        let enabled = enabled_count == total;
        // Pull a representative description from the first
        // tool in the group; the per-toolset `description`
        // field does not exist in `ToolEntryInfo`, so we
        // synthesise a stable human label. The Settings UI
        // shows the tool-name chips regardless, so this label
        // is the only thing the description slot is used for.
        let description = format!("{} category — {} tools", ts_name, total);
        out.push(ToolsetInfo {
            name: ts_name.clone(),
            label: ts_name,
            description,
            enabled,
            configured: true,
            tools,
        });
    }
    Json(out)
}

// ---------------------------------------------------------------------------
// Toolset toggle (runtime enable/disable)
// ---------------------------------------------------------------------------

/// On-disk format for the user's toolset enable/disable
/// preferences. Persisted to
/// `$HERMIX_HOME/enabled_toolsets.json`. We only store the
/// *enabled* list; the absence of a toolset name from this list
/// is what means "disabled" — keeping the file small and the
/// merge logic obvious. The "all"/"*" wildcard that the startup
/// path accepts is *not* persisted (it is a startup-only
/// convenience); a file that contains `"enabled": ["*"]` is
/// treated as "every toolset enabled".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnabledToolsetState {
    #[serde(default)]
    enabled: Vec<String>,
}

fn toolset_state_path(state: &HttpServerState) -> std::path::PathBuf {
    state.hermix_home.join("enabled_toolsets.json")
}

/// Path-only variant of [`load_toolset_state`] for use during
/// `HttpServerState::build_with_hermes_home` (when the
/// `HttpServerState` is still being constructed). Kept `pub` so
/// `lib.rs` can call it without having to thread the
/// half-built state through.
pub fn load_toolset_state_for_home(hermix_home: &Path) -> EnabledToolsetState {
    let path = hermix_home.join("enabled_toolsets.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        // File doesn't exist — this is a fresh install / first run.
        // Default to wildcard "all tools enabled" so the user doesn't
        // get an empty tool list on their very first chat. Once they
        // toggle any toolset in Settings, the file is written with an
        // explicit list.
        Err(_) => {
            tracing::info!(
                path = %path.display(),
                "enabled_toolsets.json not found — defaulting to all tools enabled"
            );
            return EnabledToolsetState {
                enabled: vec!["*".to_string()],
            };
        }
    };
    match serde_json::from_str::<EnabledToolsetState>(&data) {
        Ok(v) => {
            // An empty enabled list means "nothing enabled" — which
            // can happen when a previous version of the Settings UI
            // wrote `{"enabled":[]}` to disk. Treat it the same as
            // the wildcard: all tools enabled. The user can still
            // explicitly disable toolsets in Settings.
            if v.enabled.is_empty() {
                tracing::info!(
                    path = %path.display(),
                    "enabled_toolsets.json has empty list — defaulting to all tools enabled"
                );
                return EnabledToolsetState {
                    enabled: vec!["*".to_string()],
                };
            }
            v
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "enabled_toolsets.json is corrupt; ignoring and starting fresh"
            );
            EnabledToolsetState::default()
        }
    }
}

/// Apply the user's persisted toolset preferences to the given
/// registry. Called from `HttpServerState::build_with_hermes_home`
/// right after `register_builtin_tools` so the in-process state
/// matches what the user last persisted in
/// `enabled_toolsets.json` — otherwise the dashboard UI would
/// show the user's saved preferences on the *Settings* tab but
/// the LLM would see the default (everything-on) state until the
/// user re-toggled every toolset.
/// Toolset names not present in the persisted file are flipped
/// to `is_enabled=false` so a *new* toolset added in a later
/// binary version starts out off (acceptable — the user has not
/// seen it in the UI yet, and toggling it on costs one click).
/// A file containing `"all"` or `"*"` in the enabled list is
/// treated as "all currently-registered toolsets enabled",
/// matching the wildcard semantics from the startup
/// `register_builtin_tools` path.
pub fn apply_persisted_toolset_state(
    registry: &hermix_tools::ToolRegistry,
    hermix_home: &Path,
) {
    let persisted = load_toolset_state_for_home(hermix_home);
    let wildcard = persisted
        .enabled
        .iter()
        .any(|n| n == "all" || n == "*");
    let registered: HashSet<String> = registry.list_toolsets().into_iter().collect();
    let mut applied = 0usize;
    for ts in &registered {
        let on = wildcard || persisted.enabled.iter().any(|n| n == ts);
        let n = registry.set_toolset_enabled(ts, on);
        if on {
            applied += 1;
        }
        tracing::info!(
            toolset = %ts,
            entries = n,
            initial_enabled = on,
            "applied persisted enabled_toolsets.json"
        );
    }
    tracing::info!(
        applied_enabled = applied,
        total = registered.len(),
        "toolset startup state reconciled from enabled_toolsets.json"
    );

    // Essential toolset floor — runs AFTER the whitelist
    // reconciliation above. Optional toolsets are governed by the
    // user's `enabled_toolsets.json`; the HARD floor (terminal, file,
    // code_execution, web, memory, pdf, core, security,
    // security_guidance, custom_app) is always re-enabled so a
    // hand-edited or partially migrated list containing only
    // `["file", "web"]` cannot leave the LLM answering "I don't have
    // terminal". The SOFT floor (document, office, browser, cua,
    // storage) is re-enabled only when the list is the default
    // wildcard, so an explicit disable is respected. See
    // `hermix_tools::essential` for the full rationale.
    // `soft_floor_active = wildcard`: when the saved whitelist is the
    // default `["*"]`, fill in the soft essentials too (fresh install
    // boots fully-capable). When the user has an explicit list, the
    // soft tier is NOT re-asserted, so a soft toolset they left out
    // stays disabled (explicit disable respected — no override warning).
    let reasserted = hermix_tools::essential::ensure_essentials_enabled(registry, wildcard);
    if reasserted > 0 {
        // Only a HARD essential being overridden (user explicitly
        // disabled a hard floor toolset in a non-wildcard list) is worth
        // warning about. Soft toolsets are intentionally left to the
        // user's choice and never produce this warning.
        let overridden: Vec<&str> = hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS
            .iter()
            .copied()
            .filter(|ts| !wildcard && !persisted.enabled.iter().any(|n| n == ts))
            .collect();
        if !overridden.is_empty() {
            tracing::warn!(
                reasserted_entries = reasserted,
                overridden_toolsets = ?overridden,
                "hard-essential toolset floor overrode whitelist: the listed \
                 toolsets were re-enabled because the product cannot run \
                 safely without them. Edit HARD_ESSENTIAL_TOOLSETS in \
                 hermix_tools::essential if this is no longer desired."
            );
        }
    }
}

fn save_toolset_state(
    state: &HttpServerState,
    body: &EnabledToolsetState,
) -> Result<(), String> {
    let path = toolset_state_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(body).map_err(|e| e.to_string())?;
    // Atomic-ish write: write to a sibling tmp file, then
    // rename. Avoids leaving a half-written `enabled_toolsets.json`
    // if the process is killed mid-write. The rename is atomic
    // on the same filesystem on both Windows and Unix.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build a canonical enabled-set from a fresh toggle call.
/// "Enabled wins" semantics: the `enabled` list is the post-toggle
/// set; any name in `disabled` that is *not* in `enabled` is also
/// disabled. Names that appear in neither list are flipped to
/// disabled (because we are now in an explicit-state world; this
/// prevents stale UI state from silently keeping a toolset on
/// after the user dismissed it).
fn canonical_enabled_set(toggle: &ToolsetToggle) -> HashSet<String> {
    let set: HashSet<String> = toggle.enabled.iter().cloned().collect();
    for name in &toggle.disabled {
        if !set.contains(name) {
            // explicit disable wins over implicit enable
            // (defensive — should not happen in practice)
        }
    }
    set
}

/// PUT /api/tools/toolsets/enabled
/// Apply the user's toolset enable/disable choices to the
/// in-process `ToolRegistry` *and* persist them to
/// `enabled_toolsets.json`. Idempotent: re-sending the same
/// payload is a no-op. Unknown toolset names (typos, removed
/// toolsets) are reported in the response so the UI can show a
/// warning instead of silently ignoring them.
pub async fn set_enabled_toolsets(
    State(state): State<HttpServerState>,
    Json(body): Json<ToolsetToggle>,
) -> Result<Json<ToolsetToggleResult>, HttpError> {
    let enabled = canonical_enabled_set(&body);
    let registered: HashSet<String> =
        state.tool_registry.list_toolsets().into_iter().collect();

    // Separate requested names into known vs unknown before we
    // touch the registry.
    let mut unknown: Vec<String> = Vec::new();
    for name in enabled.iter().chain(body.disabled.iter()) {
        if !registered.contains(name) {
            unknown.push(name.clone());
        }
    }
    unknown.sort();
    unknown.dedup();

    // The desired state: every registered toolset whose name
    // is in `enabled` is on; everything else is off. This
    // makes the call a "set absolute state" rather than a
    // diff, which is the contract the UI expects.
    // (P1.7): the helper `apply_toggle_with_floor` (defined
    // below) does the toggle *and* re-asserts the essential toolset
    // floor in one pass. Without the floor re-assert, a user can
    // use the Settings UI to toggle `terminal` off; the in-process
    // state then has it disabled, and the next `set_enabled_toolsets`
    // call persists the disabled state to `enabled_toolsets.json`.
    // The next process restart would load the persisted file, see
    // `terminal` missing from the enabled list, and
    // `apply_persisted_toolset_state` would (correctly) re-enable it
    // — *but the running process is now broken until restart*. We
    // close the gap by applying the floor here, the same way the
    // startup paths do. The trade-off (see `hermix_tools::essential`
    // for the full rationale): the UI may show a toolset as "off"
    // briefly, but the in-process state always matches what the
    // LLM will be told exists. We also extend the persisted list
    // to include the re-asserted floor, so a restart doesn't undo
    // the override.
    let new_enabled =
        apply_toggle_with_floor(&state.tool_registry, &registered, &enabled);

    // Persist. We save the *enabled* list (the disabled set is
    // derivable as the complement of `registered`, which the
    // server rebuilds on every restart). This means a
    // downgrade of the binary that adds a new toolset will
    // start with that new toolset *off* — acceptable, since
    // the user has not yet seen it in the UI.
    let to_save = EnabledToolsetState {
        enabled: new_enabled.clone(),
    };
    if let Err(e) = save_toolset_state(&state, &to_save) {
        return Err(HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to persist enabled_toolsets.json: {}", e),
        });
    }

    Ok(Json(ToolsetToggleResult {
        enabled: new_enabled,
        unknown,
    }))
}

/// Apply a "set absolute state" toggle to a [`ToolRegistry`] and
/// re-assert the essential toolset floor in the same pass. Returns
/// the canonical enabled list (sorted, with the floor folded in)
/// that the caller should both report to the UI and persist to
/// `enabled_toolsets.json`.
/// Why one helper, not two? The floor re-assertion is part of the
/// toggle contract: a user-facing toggle of `terminal` to "off"
/// is overridden by the floor, and that override has to be
/// reflected in **both** the in-process state *and* the persisted
/// file. Splitting them into two steps is what made the runtime
/// path silently drop the floor (P1.7 bug) — the floor was
/// applied at startup but not at toggle time, and the toggle
/// path did not see a reason to consult the floor.
/// Splitting out a helper also makes the toggle logic testable
/// without spinning up the full `HttpServerState` (which the
/// test module in this file already does for the seed path).
fn apply_toggle_with_floor(
    registry: &hermix_tools::ToolRegistry,
    registered: &HashSet<String>,
    requested_enabled: &HashSet<String>,
) -> Vec<String> {
    let mut new_enabled: Vec<String> = Vec::new();
    for ts in registered {
        let on = requested_enabled.contains(ts);
        let n = registry.set_toolset_enabled(ts, on);
        if on {
            new_enabled.push(ts.clone());
            tracing::info!(toolset = %ts, entries = n, "toolset enabled at runtime");
        } else {
            tracing::info!(toolset = %ts, entries = n, "toolset disabled at runtime");
        }
    }

    // The user is actively curating their toolset list here, so the soft
    // floor is NOT applied — a soft toolset the user turned off stays off
    // (explicit disable respected). Only the hard floor is enforced.
    let reasserted = hermix_tools::essential::ensure_essentials_enabled(registry, false);
    if reasserted > 0 {
        for ts in hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS {
            if registered.contains(*ts) && !new_enabled.iter().any(|n| n == ts) {
                new_enabled.push(ts.to_string());
            }
        }
        tracing::warn!(
            reasserted_entries = reasserted,
            floor = ?hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS,
            "runtime toolset toggle overrode hard-essential floor; re-enabling \
             and persisting the floor-applied state so a restart sees the \
             same set."
        );
    }
    new_enabled.sort();
    new_enabled
}

// ── Builtin skill seeding ────────────────────────────────────────────
// The 15 .md files in `dashboard/skills/` are bundled into the binary at
// compile time. On first start we materialise them into the user's skills
// directory (`~/.hermix/skills/<category>/<name>/SKILL.md`) so the front-end
// "扩展 → 技能" tab has a populated list right away.
// This is **idempotent** — we never overwrite a SKILL.md the user has
// already customised. To restore a builtin skill, the user must delete its
// directory first.

/// `(category, name, raw .md content)` — the .md files include YAML
/// frontmatter that we use as the source of truth for category/name, so the
/// tuple here is just the dispatch index.
const BUILTIN_SKILLS: &[(&str, &str, &str)] = &[
    // (category, skill name, embedded .md content)
    (
        "software-development",
        "architecture-design",
        include_str!("skills/architecture-design.md"),
    ),
    (
        "software-development",
        "api-design",
        include_str!("skills/api-design.md"),
    ),
    (
        "software-development",
        "code-review",
        include_str!("skills/code-review.md"),
    ),
    (
        "software-development",
        "database-design",
        include_str!("skills/database-design.md"),
    ),
    (
        "software-development",
        "frontend-architecture",
        include_str!("skills/frontend-architecture.md"),
    ),
    (
        "software-development",
        "debugging-methodology",
        include_str!("skills/debugging-methodology.md"),
    ),
    (
        "software-development",
        "tdd-practice",
        include_str!("skills/tdd-practice.md"),
    ),
    (
        "devops",
        "devops-practice",
        include_str!("skills/devops-practice.md"),
    ),
    (
        "productivity",
        "plan-writing",
        include_str!("skills/plan-writing.md"),
    ),
    (
        "productivity",
        "writing-assistant",
        include_str!("skills/writing-assistant.md"),
    ),
    (
        "productivity",
        "learning-coach",
        include_str!("skills/learning-coach.md"),
    ),
    (
        "finance",
        "investment-analysis",
        include_str!("skills/investment-analysis.md"),
    ),
    (
        "finance",
        "finance-planning",
        include_str!("skills/finance-planning.md"),
    ),
    (
        "lifestyle",
        "travel-planning",
        include_str!("skills/travel-planning.md"),
    ),
    (
        "lifestyle",
        "health-wellness",
        include_str!("skills/health-wellness.md"),
    ),
    (
        "writing",
        "web-novel",
        include_str!("skills/web-novel.md"),
    ),
    // office-cli: re-enabled (P1.8) — the `office_cli` builtin
    // tool was re-registered in register_builtins.rs and the desktop preview
    // (render_office_preview) shells out to the bundled `officecli` binary,
    // so this skill is live again. See hermix-tools register_builtins.rs.
    (
        "office",
        "office-cli",
        include_str!("skills/office/office-cli/SKILL.md"),
    ),
    // custom-app-development: hx-shell 自定义应用开发指南（含真实 schema / API / 图标库），
    // 内置为 seed 以便分发到其它机器；单一真相位置定为 "hermix" 分类（与 store 动态扫描
    // 所有子目录的读取机制一致；运行期副本只在 ~/.hermix/skills/hermix/ 一份）。
    (
        "hermix",
        "custom-app-development",
        include_str!("skills/custom-app-development.md"),
    ),
];

/// Materialise the bundled builtin skills into `skills_dir`. Returns the
/// number of files actually written (i.e. fresh installs on first run).
/// Idempotent: a SKILL.md that already exists is left alone so user edits
/// survive. To re-seed a specific skill, the user must delete its
/// `<category>/<name>/SKILL.md` file first.
pub fn seed_builtin_skills(skills_dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(skills_dir)?;

    let mut written = 0;
    for (category, name, content) in BUILTIN_SKILLS {
        let dir: PathBuf = skills_dir.join(category).join(name);
        let path = dir.join("SKILL.md");
        if path.exists() {
            continue; // user already customised / previously seeded
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, content)?;
        written += 1;
        tracing::info!(
            "seeded builtin skill: {} ({}/{})",
            name,
            category,
            name
        );
    }
    Ok(written)
}

/// Helper for `lib.rs`: seed the default skills directory. Idempotent.
pub fn seed_builtin_skills_default() -> usize {
    let dir = hermix_skills::FileSkillStore::default_dir();
    match seed_builtin_skills(&dir) {
        Ok(n) if n > 0 => {
            tracing::info!("Seeded {} builtin skills into {}", n, dir.display());
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Failed to seed builtin skills: {}", e);
        }
    }
    // n is informational only; the actual count is logged above.
    BUILTIN_SKILLS.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_skills::SkillStore;
    use tempfile::tempdir;

    #[test]
    fn seed_builtin_skills_writes_all_skills() {
        let dir = tempdir().unwrap();
        let count = seed_builtin_skills(dir.path()).unwrap();
        assert_eq!(count, BUILTIN_SKILLS.len(), "should seed all builtin skills");
        // Verify some exist
        assert!(
            dir.path().join("software-development").join("architecture-design").join("SKILL.md").exists(),
            "architecture-design SKILL.md should exist"
        );
        assert!(
            dir.path().join("finance").join("investment-analysis").join("SKILL.md").exists(),
            "investment-analysis SKILL.md should exist"
        );
        assert!(
            dir.path().join("office").join("office-cli").join("SKILL.md").exists(),
            "office-cli SKILL.md should exist (re-enabled in P1.8)"
        );
        // Idempotent: second call writes 0
        let second = seed_builtin_skills(dir.path()).unwrap();
        assert_eq!(second, 0, "second seed should write nothing");
    }

    #[tokio::test]
    async fn file_skill_store_load_by_name() {
        let dir = tempdir().unwrap();
        seed_builtin_skills(dir.path()).unwrap();

        let store = hermix_skills::FileSkillStore::new(dir.path().to_path_buf());

        // Load a skill by name only (no category needed)
        let skill = store.load("architecture-design").await.unwrap()
            .expect("architecture-design should be found by name alone");
        assert_eq!(skill.name, "architecture-design");
        assert!(skill.content.contains("## 核心原则"), "content should have markdown body");
        assert_eq!(skill.category.as_deref(), Some("software-development"));
        assert!(skill.description.as_deref().unwrap_or("").contains("单体 vs 微服务"));

        // Non-existent name returns None
        let missing = store.load("no-such-skill").await.unwrap();
        assert!(missing.is_none(), "unknown skill should return None");
    }

    #[tokio::test]
    async fn file_skill_store_load_from_all_categories() {
        let dir = tempdir().unwrap();
        seed_builtin_skills(dir.path()).unwrap();
        let store = hermix_skills::FileSkillStore::new(dir.path().to_path_buf());

        // Skills across different categories — all loadable by plain name
        for name in &["architecture-design", "devops-practice", "plan-writing", "investment-analysis", "travel-planning"] {
            let skill = store.load(name).await.unwrap()
                .unwrap_or_else(|| panic!("{name} should be loadable by name"));
            assert_eq!(&skill.name, name);
        }
    }

    #[tokio::test]
    async fn list_skills_returns_seeded_skills() {
        let dir = tempdir().unwrap();
        seed_builtin_skills(dir.path()).unwrap();
        let store = hermix_skills::FileSkillStore::new(dir.path().to_path_buf());
        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), BUILTIN_SKILLS.len(), "list should return all seeded skills");
        // Verify names from each category
        let names: Vec<&str> = metas.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"architecture-design"));
        assert!(names.contains(&"devops-practice"));
        assert!(names.contains(&"plan-writing"));
        assert!(names.contains(&"investment-analysis"));
        assert!(names.contains(&"travel-planning"));
        assert!(names.contains(&"web-novel"));
    }

    // ── P1.7: essential-floor override at runtime ────────────────────
    // Regression: the floor was applied at startup (twice: once in
    // `register_builtin_tools` and once in
    // `apply_persisted_toolset_state`) but NOT in
    // `set_enabled_toolsets`. A user could use the Settings UI to
    // toggle `terminal` off; the in-process state would then have
    // it disabled, and the next LLM call would 4xx the terminal
    // tool — even though the floor docstring in
    // `hermix_tools::essential` promised the LLM would always see
    // it. The fix: `apply_toggle_with_floor` re-asserts the floor
    // and folds the re-enabled toolsets into the returned list, so
    // a restart sees the same effective state.
    // Test strategy: `ToolRegistry` is a closed shape (no public
    // `is_toolset_enabled` getter; `set_toolset_enabled` returns
    // the *number of flipped entries*, not the new state). We
    // exercise the floor override behaviour against a real
    // registry built by `register_builtin_tools` and observe the
    // *returned list*, which is the contract callers care about:
    // it must include the floor toolsets regardless of the user's
    // request.

    use hermix_core::{CommandOutput, SkillProvider, TerminalBackend};
    use std::sync::Arc;

    /// No-op skill provider so `register_builtin_tools` accepts
    /// our `Arc<dyn SkillProvider>` argument. The runtime toggle
    /// path doesn't touch it; we just need the registry to come
    /// up with the canonical toolset list.
    struct NoopSkillProvider;
    #[async_trait::async_trait]
    impl SkillProvider for NoopSkillProvider {
        async fn create_skill(
            &self,
            _name: &str,
            _content: &str,
            _category: Option<&str>,
        ) -> Result<hermix_core::Skill, hermix_core::AgentError> {
            //( / ): return a structured error
            // instead of unreachable!() so a routing mistake degrades
            // gracefully instead of panicking the dashboard.
            Err(hermix_core::AgentError::ToolExecution(
                "NoopSkillProvider::create_skill should never be called".into()
            ))
        }
        async fn get_skill(
            &self,
            _name: &str,
        ) -> Result<Option<hermix_core::Skill>, hermix_core::AgentError> {
            Ok(None)
        }
        async fn list_skills(
            &self,
        ) -> Result<Vec<hermix_core::SkillMeta>, hermix_core::AgentError> {
            Ok(vec![])
        }
        async fn update_skill(
            &self,
            _name: &str,
            _content: &str,
        ) -> Result<hermix_core::Skill, hermix_core::AgentError> {
            Err(hermix_core::AgentError::ToolExecution(
                "NoopSkillProvider::update_skill should never be called".into()
            ))
        }
        async fn delete_skill(
            &self,
            _name: &str,
        ) -> Result<(), hermix_core::AgentError> {
            Ok(())
        }
    }

    /// No-op terminal backend. `register_builtin_tools` only uses
    /// it to construct `TerminalHandler`; the test itself never
    /// dispatches a terminal call.
    struct NoopBackend;
    #[async_trait::async_trait]
    impl TerminalBackend for NoopBackend {
        async fn execute_command(
            &self,
            _cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, hermix_core::AgentError> {
            Err(hermix_core::AgentError::ToolExecution(
                "NoopBackend::execute_command should never be called".into()
            ))
        }
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, hermix_core::AgentError> {
            Err(hermix_core::AgentError::ToolExecution(
                "NoopBackend::read_file should never be called".into()
            ))
        }
        async fn write_file(
            &self,
            _path: &str,
            _content: &str,
        ) -> Result<(), hermix_core::AgentError> {
            Err(hermix_core::AgentError::ToolExecution(
                "NoopBackend::write_file should never be called".into()
            ))
        }
        async fn file_exists(
            &self,
            _path: &str,
        ) -> Result<bool, hermix_core::AgentError> {
            Err(hermix_core::AgentError::ToolExecution(
                "NoopBackend::file_exists should never be called".into()
            ))
        }
    }

    fn fixture_registry() -> (hermix_tools::ToolRegistry, HashSet<String>) {
        // Isolate HERMIX_HOME so the BrowserConfig loader doesn't
        // pick up the dev machine's real ~/.hermix/config.yaml —
        // a tempdir is the only safe way to make the fixture
        // hermetic. (See hermix_config::paths::hermix_home().)
        let home = tempdir().unwrap();
        // 守卫：离开时把 HERMIX_HOME 还原。此前裸 set_var 会让这个临时目录
        // 被删之后仍被并行测试读到（pairing 测试写 TLS 证书偶发 os error 2）。
        let _home_guard = crate::test_env::EnvGuard::set("HERMIX_HOME", home.path());
        let reg = hermix_tools::ToolRegistry::new();
        let skill_provider: Arc<dyn SkillProvider> = Arc::new(NoopSkillProvider);
        let terminal: Arc<dyn TerminalBackend> = Arc::new(NoopBackend);
        hermix_tools::register_builtin_tools(&reg, terminal, skill_provider, None, None, None, None, None);
        let registered: HashSet<String> =
            reg.list_toolsets().into_iter().collect();
        // Sanity: at least the *core* floor must be present
        // (`terminal` is the documented regression test). We don't
        // hard-pin every entry in `ESSENTIAL_TOOLSETS` because
        // the constant has historically out-paced registration
        // (e.g. `pdf`, `office`, `computer_use`, `cua` are in
        // the floor but not in the runtime registry — a separate
        // P-bug tracked elsewhere). The P1.7 contract is "floor
        // toolsets that *are* registered must survive a runtime
        // toggle-off", which is what the tests below check.
        assert!(
            registered.iter().any(|n| n == "terminal"),
            "P1.7 fixture: `terminal` must be in registry, got {registered:?}"
        );
        assert!(
            registered.iter().any(|n| n == "file"),
            "P1.7 fixture: `file` must be in registry, got {registered:?}"
        );
        (reg, registered)
    }

    #[test]
    fn apply_toggle_with_floor_keeps_essentials_even_when_user_toggles_off() {
        let (reg, registered) = fixture_registry();

        // User toggles everything off — including `terminal` and
        // `file`, which are in the essential floor. The `requested`
        // set intentionally contains *no* floor toolsets.
        let requested: HashSet<String> = HashSet::new();
        let returned = apply_toggle_with_floor(&reg, &registered, &requested);

        // Persisted list: the floor is folded in, so a restart
        // sees the same effective state. This is the observable
        // contract of the helper; the in-process state is mirrored
        // exactly (we call `set_toolset_enabled` on the same
        // registry) but the public API doesn't expose a getter to
        // assert on, so we test through the returned list.
        for ts in hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS {
            if registered.iter().any(|n| n == ts) {
                assert!(
                    returned.iter().any(|n| n == ts),
                    "P1.7 regression: returned list must include hard floor toolset {ts} \
                     even when user requested everything off. Got: {returned:?}"
                );
            }
        }
        // Returned list is sorted (contract callers rely on).
        let mut sorted = returned.clone();
        sorted.sort();
        assert_eq!(returned, sorted, "returned list must be sorted");
    }

    #[test]
    fn apply_toggle_with_floor_is_idempotent_when_user_already_kept_essentials() {
        let (reg, registered) = fixture_registry();
        // User requests the full floor plus `browser`. The
        // helper should be a near-no-op (no re-assertion needed).
        let requested: HashSet<String> = registered.clone();
        let returned = apply_toggle_with_floor(&reg, &registered, &requested);
        // The returned list must at minimum contain every
        // registered toolset (no floor override needed, no extras
        // dropped).
        for ts in &registered {
            assert!(
                returned.contains(ts),
                "P1.7: requested-full toggle must preserve toolset {ts} in returned list"
            );
        }
        // Defensive: the returned set must equal the requested
        // set (the floor is a subset of requested, so no
        // additions). Sort both into Vec for a deterministic
        // comparison (HashSet itself has no `sort`).
        let mut req_vec: Vec<String> = requested.into_iter().collect();
        req_vec.sort();
        assert_eq!(
            returned, req_vec,
            "P1.7: when user requests everything, returned should mirror it"
        );
    }

    #[test]
    fn apply_toggle_with_floor_keeps_essentials_when_user_passes_partial_set() {
        let (reg, registered) = fixture_registry();
        // User keeps only `web`. Floor must re-assert. We pick
        // `web` because it is both in the floor *and* registered
        // (the floor lists `browser` too, but P1.3 split the
        // browser toolset into per-action toolsets
        // `browser_navigate`/`browser_click`/etc., so the
        // single-name `browser` no longer exists in the runtime
        // registry).
        let mut requested: HashSet<String> = HashSet::new();
        requested.insert("web".to_string());
        let returned = apply_toggle_with_floor(&reg, &registered, &requested);
        for ts in hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS {
            if registered.iter().any(|n| n == ts) {
                assert!(
                    returned.iter().any(|n| n == ts),
                    "P1.7: partial-user toggle must still re-include hard floor toolset {ts}"
                );
            }
        }
        assert!(
            returned.contains(&"web".to_string()),
            "P1.7: user's `web` choice must be honoured"
        );
    }

    #[test]
    fn apply_toggle_with_floor_respects_explicit_soft_disable() {
        let (reg, registered) = fixture_registry();
        // User keeps only `web` (a hard essential). Any soft essential
        // that is registered must NOT be force-re-enabled by the floor —
        // an explicit disable must be respected. This is the
        // anti-regression test for the "essential toolset floor overrode
        // whitelist" warning that used to fire on startup for `document`.
        let mut requested: HashSet<String> = HashSet::new();
        requested.insert("web".to_string());
        let returned = apply_toggle_with_floor(&reg, &registered, &requested);

        for ts in hermix_tools::essential::SOFT_ESSENTIAL_TOOLSETS {
            if registered.iter().any(|n| n == ts) {
                assert!(
                    !returned.iter().any(|n| n == ts),
                    "soft essential {ts} must stay off when the user toggled it \
                     off and did not request it. Got: {returned:?}"
                );
            }
        }
        // Hard essentials still survive the toggle-off.
        for ts in hermix_tools::essential::HARD_ESSENTIAL_TOOLSETS {
            if registered.iter().any(|n| n == ts) {
                assert!(
                    returned.iter().any(|n| n == ts),
                    "hard essential {ts} must survive an explicit toggle-off"
                );
            }
        }
    }
}
