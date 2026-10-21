//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::collections::HashSet;
use std::sync::Arc;

use hermix_core::{SkillProvider, TerminalBackend};

use crate::tools::clarify::ClarifyBackend;
use crate::ToolRegistry;

/// Register all built-in tool handlers into the given registry.
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
/// `clarify_backend` backs the `clarify` tool. Callers that want a
/// blocking user-prompt experience (e.g. the desktop app via
/// [`ChannelClarifyBackend`](crate::backends::clarify::ChannelClarifyBackend))
/// should pass it here; otherwise the default [`SignalClarifyBackend`](crate::backends::clarify::SignalClarifyBackend)
/// returns a JSON envelope without waiting.
/// Backends that depend on environment variables (e.g. `OPENAI_API_KEY`,
/// `FAL_KEY`, `HASS_URL`) are constructed eagerly; if the vars are absent
/// the tools will return a clear error at *runtime*, not at registration.
/// The body is an honest pipeline: register_all → clarify override →
/// post-passes (whitelist + essential floor).
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    clarify_backend: Option<Arc<dyn ClarifyBackend>>,
    enabled_toolsets: Option<HashSet<String>>,
    todo_event_tx: Option<tokio::sync::broadcast::Sender<crate::tools::todo::TodoChange>>,
    custom_app_manager: Option<Arc<hermix_plugin::PluginManager>>,
    // 接缝：主进程装配（hermix-context::boot）注入 fs/shell。`None` 时
    // 退回 file/terminal 的老路径。tests + 没装配的调用方都传 None。
    seams: Option<hermix_context::BootSeams>,
) {
    // 1. Clarify backend. Caller-provided (desktop uses
    // `ChannelClarifyBackend` for prompt-the-user) wins over the
    // signal-based default. The default is used by tests and CLI
    // callers that don't need a UI prompt.
    let clarify_backend: Arc<dyn ClarifyBackend> = match clarify_backend {
        Some(b) => b,
        None => Arc::new(crate::backends::clarify::SignalClarifyBackend::new()),
    };

    // 2. Wire everything into the registry via the per-tool
    // `register(...)` functions. Then re-register `clarify` on
    // top with the caller-provided backend so the desktop's
    // blocking prompt overrides the signal default that
    // `register_all` just installed.
    crate::tools::register_all(
        registry,
        terminal_backend,
        skill_provider,
        todo_event_tx,
        seams,
    );
    crate::tools::clarify::register(registry, clarify_backend);
    // Mark interactive tools that must not run in parallel with others.
    registry.mark_interactive("clarify");
    registry.mark_interactive("delegate_task");

    // Custom app manager — LLM tool for creating user-facing desktop apps.
    if let Some(mgr) = custom_app_manager {
        crate::tools::custom_app_manager::register(registry, mgr);
    }

    // Workbench manager — bridge to hermix-plugin::workbench (code file
    // read/write with lint gate, module index, deps, undo). Always
    // registered: the workbench SKILL.md (pinned into workbench sessions
    // via AgentConfig::ephemeral_skills, MODULE_DESIGN §13 layer 3)
    // teaches exactly these operations, so the tool table must expose
    // them or the agent invents tool names.
    crate::tools::workbench_manager::register(registry);

    // Agent BBS 工具 — bbs_read（Safe）/ bbs_post（Caution，外部可见）。
    // 服务部署在 relay 同机（/bbs/* 反代）；key 走 env HERMIX_BBS_KEY，
    // 未配置时工具照常注册（同 homeassistant 先例），执行时如实报缺 key。
    crate::tools::bbs::register(registry);

    // Vault（凭据保险库）— vault_list / vault_get。打开失败（如 CredMan
    // 不可用）时跳过注册而不是崩进程：agent 少两个工具，其余照常工作。
    // 信任边界：工具层不做二次鉴权（RiskLevel::Safe 只读）；手机/IM 通道
    // 是否放行由通道侧设备配对判定（决策笔记 Consequences 有记）。
    match hermix_vault::Vault::open() {
        Ok(vault) => {
            crate::tools::vault::register(registry, std::sync::Arc::new(vault));
        }
        Err(e) => {
            tracing::warn!(error = %e, "vault 不可用，跳过 vault_list/vault_get 注册");
        }
    }

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());

    // 4. Whitelist post-pass. `tools::register_all` registers every
    // tool unconditionally; this flips the ones the user did not
    // whitelist back off, restoring the Settings UI's runtime
    // toggle without re-registering. Mirrored wildcard (`"all"` /
    // `"*"`) keeps "user wants everything on" working.
    if let Some(set) = &enabled_toolsets {
        let wildcard = set.contains("all") || set.contains("*");
        if !wildcard {
            for ts in registry.list_toolsets() {
                if !set.contains(&ts) {
                    let n = registry.set_toolset_enabled(&ts, false);
                    tracing::info!(
                        toolset = %ts,
                        entries = n,
                        "startup: toolset disabled (not in enabled_toolsets)"
                    );
                }
            }
        }
    }

    // 5. Essential toolset floor (hard tier only). All tools register
    // enabled, so this is a no-op safety net: a hard-essential toolset
    // can never end up disabled at the end of registration. The soft
    // tier is intentionally NOT applied here — the user's whitelist
    // (`apply_persisted_toolset_state`) is the authority on optional
    // toolsets and will turn off any soft toolset the user left out.
    // See `essential::ensure_essentials_enabled` for the full rationale.
    let reasserted = crate::essential::ensure_essentials_enabled(registry, false);
    if reasserted > 0 {
        tracing::info!(
            entries = reasserted,
            toolset = ?crate::essential::HARD_ESSENTIAL_TOOLSETS,
            "startup: re-enabled hard-essential toolset floor (whitelist did not contain it)"
        );
    }

    // 6. Opt-in toolsets (native OS automation: `desktop`).
    // Runs last on purpose — steps 4 and 5 both *enable* toolsets, so
    // anything earlier would be undone. This ships off even under the
    // `["*"]` wildcard a fresh install starts from: driving the user's
    // real desktop is not something a default install should do to a
    // machine the operator has not opted in.
    // (`browser` was here too, but the virtual-window P0 removed the
    // "drives the user's real browser" concern; it now lives on the
    // essential hard floor — see `essential::HARD_ESSENTIAL_TOOLSETS`.)
    // `hermix-server::dashboard::skills::apply_persisted_toolset_state`
    // calls the same helper with the persisted list, so the two paths
    // cannot drift.
    let opt_in_off =
        crate::tools::os_auto::apply_opt_in_defaults(registry, enabled_toolsets.as_ref());
    if opt_in_off > 0 {
        tracing::info!(
            toolsets = opt_in_off,
            "startup: opt-in toolsets left disabled until explicitly enabled in Settings"
        );
    }
}

// `hermes_data_dir` / `dirs_home` were deleted: they bypassed
// `hermix_config::paths::hermix_home()` and ignored `HERMIX_PORTABLE_DATA_DIR`,
// which caused session_search to write to a different DB than the rest of
// the app in portable-desktop mode. Use `paths::sessions_db_path()` instead.

#[cfg(test)]
mod tests {
    //! Regression tests for `register_builtin_tools`.
    //!
    //! The `terminal` tool *must* register as `RiskLevel::Caution`, not
    //! `Dangerous`. Marking it `Dangerous` causes
    //! `local_agent_service::on_pre_tool_dispatch` to fire
    //! `ToolDispatchDecision::AwaitApproval` for every terminal call —
    //! including the obvious read-only cases (`ls -la`, `echo hello`,
    //! `cat file.txt`). The user has explicitly called this out as a
    //! false positive in the desktop UI. The per-call risk is correctly
    //! decided inside `TerminalHandler::execute` by the inner
    //! `ApprovalManager` (which already denies `rm -rf /` and friends);
    //! the tool-level risk only needs to signal "may have side effects,
    //! command shape decides whether to interrupt".
    use super::*;
    use async_trait::async_trait;
    use hermix_core::{
        traits::RiskLevel, AgentError, CommandOutput, Skill, SkillMeta, SkillProvider,
        TerminalBackend,
    };
    use crate::essential::ESSENTIAL_TOOLSETS;
    use hermix_plugin::PluginManager;

    /// Minimal no-op terminal backend for `register_builtin_tools`. The
    /// backend is only used to wire handlers; we never invoke it in
    /// these tests.
    struct NoopTerminal;
    #[async_trait]
    impl TerminalBackend for NoopTerminal {
        async fn execute_command(
            &self,
            _cmd: &str,
            _t: Option<u64>,
            _w: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
        async fn read_file(
            &self,
            _p: &str,
            _o: Option<u64>,
            _l: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
        async fn write_file(&self, _p: &str, _c: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _p: &str) -> Result<bool, AgentError> {
            Ok(false)
        }
    }

    /// Minimal no-op skill provider.
    struct NoopSkills;
    #[async_trait]
    impl SkillProvider for NoopSkills {
        async fn create_skill(
            &self,
            _name: &str,
            _content: &str,
            _category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            // NoopSkills is a test mock that does not support skill
            // creation. Return a proper error instead of panicking.
            Err(AgentError::ToolExecution(
                "NoopSkills does not support create_skill".into(),
            ))
        }
        async fn get_skill(&self, _name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(None)
        }
        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(Vec::new())
        }
        async fn update_skill(
            &self,
            _name: &str,
            _content: &str,
        ) -> Result<Skill, AgentError> {
            // NoopSkills is a test mock that does not support skill
            // updates. Return a proper error instead of panicking.
            Err(AgentError::ToolExecution(
                "NoopSkills does not support update_skill".into(),
            ))
        }
        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn terminal_tool_is_registered_as_caution_not_dangerous() {
        // The core regression check: terminal MUST be `Caution` so the
        // dispatch gate in `local_agent_service` short-circuits benign
        // read-only commands (`ls`, `cat`, `echo`) without popping a
        // channel-approval dialog. The inner `ApprovalManager` covers
        // the actually-dangerous cases (`rm -rf`, `sudo`, `mkfs`, etc.)
        // at execution time.
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(NoopTerminal),
            Arc::new(NoopSkills),
            None,
            None,
            None,
            None,
            None,
        );
        let risk = registry
            .risk_level("terminal")
            .expect("terminal tool must be registered");
        assert_eq!(
            risk,
            RiskLevel::Caution,
            "terminal must be Caution (not Dangerous). \
             Dangerous pre-prompts every terminal call, which causes \
             `ls -la` / `echo hello` to falsely trigger the user-approval \
             dialog. Real danger is gated by ApprovalManager inside the \
             handler."
        );
    }

    /// Build a registry with the real built-in tools wired to
    /// noop backends. Reused by every invariant test below so
    /// each one exercises the actual `register_builtin_tools`
    /// path, not a hand-constructed stub.
    fn registry_with_builtins() -> ToolRegistry {
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(NoopTerminal),
            Arc::new(NoopSkills),
            None,
            None,
            None,
            // Register `custom_app_manager` here (matching the production
            // `build_with_hermes_home` path, which passes `Some`). Without
            // it the `"custom_app"` toolset has no registered tool and the
            // `essential_toolset_floor_aligns_with_runtime_registration`
            // floor test fails — the floor only re-asserts toolsets that
            // actually have a wired tool.
            Some(Arc::new(PluginManager::new(
                std::env::temp_dir().join("hermix-test-custom-apps"),
            ))),
            // 接缝 `None`：tests 不需要真接缝，验证 register 路径老形状不变。
            None,
        );
        registry
    }

    /// Documented-Safe tools — read-only by design and
    /// unconditionally registered (no env-var or feature
    /// gates). If any of these regress to `Caution`/`Dangerous`,
    /// the dispatch gate will start firing unnecessary
    /// approval cards.
    /// When adding a new Safe tool: add it here. When removing
    /// one: remove the line and document the rationale.
    /// Tools that are conditionally registered (e.g. require
    /// `HASS_URL`, `OPENAI_API_KEY`, or a browser driver) are
    /// *not* listed here — they are exercised by separate
    /// integration tests in the consumer crates.
    const DOCUMENTED_SAFE_TOOLS: &[&str] = &[
        "web_search",
        "web_extract",
        "url_safety",
        "clarify",
        "model_providers",
        "platforms",
        "security_guidance",
        "skills_list",
    ];

    /// Documented-Caution tools — registered unconditionally,
    /// have internal guards but no inherent destructive power
    /// beyond what the tool's own logic does.
    const DOCUMENTED_CAUTION_TOOLS: &[&str] = &[
        "terminal",
        "delegate_task",
        "send_message",
    ];

    #[test]
    fn safe_tools_are_registered_as_safe() {
        let registry = registry_with_builtins();
        for tool in DOCUMENTED_SAFE_TOOLS {
            let risk = registry
                .risk_level(tool)
                .unwrap_or_else(|| panic!("safe tool `{}` must be registered", tool));
            assert_eq!(
                risk,
                RiskLevel::Safe,
                "documented-safe tool `{}` regressed to {:?}. \
                 Safe tools are read-only by design; promoting them to \
                 Caution / Dangerous would cause the dispatch gate to \
                 pop unnecessary approval cards. The dispatch flow is \
                 `pre-dispatch → Allow for Safe`, see \
                 local_agent_service::decide_pre_tool_dispatch.",
                tool,
                risk
            );
        }
    }

    #[test]
    fn caution_tools_are_registered_as_caution() {
        let registry = registry_with_builtins();
        for tool in DOCUMENTED_CAUTION_TOOLS {
            let risk = registry
                .risk_level(tool)
                .unwrap_or_else(|| panic!("caution tool `{}` must be registered", tool));
            assert_eq!(
                risk,
                RiskLevel::Caution,
                "documented-caution tool `{}` regressed to {:?}.",
                tool,
                risk
            );
        }
    }

    #[test]
    fn terminal_is_caution_not_safe_or_dangerous() {
        // Belt-and-suspenders: the user-visible regression
        // is "terminal is Dangerous → `ls` pops a card". The
        // opposite regression (terminal is Safe) would skip
        // the per-command `ApprovalManager` check that catches
        // `rm -rf /` etc. — actually no, the
        // `ApprovalManager` runs inside the handler, not the
        // dispatch gate, so Safe would also be safe in
        // practice. But Caution is the documented value and
        // we want a single source of truth. This test pins
        // the exact level.
        let registry = registry_with_builtins();
        let risk = registry.risk_level("terminal").expect("terminal registered");
        assert_eq!(
            risk,
            RiskLevel::Caution,
            "terminal must remain RiskLevel::Caution"
        );
    }

    #[test]
    fn every_registered_tool_has_a_unique_name() {
        // Defensive: the registry keys every tool by `name`, and the
        // LLM dispatch path looks up by `name`. A duplicate name
        // means the second registration is silently dropped (the
        // `register_with_risk(override_existing=false)` contract).
        // The intent is that every tool's `pub fn register` registers
        // exactly one entry under a unique name — this test pins
        // that invariant. If a future refactor accidentally double-
        // registers a tool, the test panics with the collision pair
        // so the developer can grep for the offending `register` call.
        let registry = registry_with_builtins();
        let entries = registry.list_tools();
        let total = entries.len();
        let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate tool names in registry (total entries = {total})");
    }

    #[test]
    fn every_registered_tool_has_a_nonempty_toolset_tag() {
        // Defensive: the Settings UI filters / groups by `toolset`.
        // A tool registered with an empty `toolset` string would be
        // un-toggleable from the UI and would silently escape the
        // `essential::ensure_essentials_enabled` floor (which keys
        // on the toolset tag). Pin every entry to a non-empty tag.
        let registry = registry_with_builtins();
        for entry in registry.list_tools() {
            assert!(
                !entry.toolset.is_empty(),
                "tool `{}` registered without a toolset tag",
                entry.name
            );
        }
    }

    #[test]
    fn toolset_members_expands_known_group_and_nops_unknown() {
        // 专家 toolset 白名单并集的数据源：已知组必须返回非空成员，
        // 未知组必须返回空（no-op，不能 panic）——受聘专家声明了
        // 已被移除的组名时不允许炸掉整轮对话。
        let registry = registry_with_builtins();
        let sets = registry.list_toolsets();
        assert!(!sets.is_empty(), "builtin registry must have toolsets");
        for ts in &sets {
            let members = registry.toolset_members(ts);
            assert!(
                !members.is_empty(),
                "toolset `{ts}` listed by list_toolsets but has no members"
            );
        }
        assert!(registry.toolset_members("no-such-toolset-group").is_empty());
    }

    #[test]
    fn every_registered_tool_has_a_nonempty_description() {
        // The catalog one-liner (`registry.catalog()`) and the WS
        // `tool_definition` event both surface the description. An
        // empty description would render as a blank line in the
        // Settings UI tool list, defeating the operator's ability
        // to recognise what the tool does.
        let registry = registry_with_builtins();
        for entry in registry.list_tools() {
            assert!(
                !entry.description.trim().is_empty(),
                "tool `{}` registered with empty description",
                entry.name
            );
        }
    }

    #[test]
    fn all_registered_tools_have_a_known_risk_level() {
        // Catches tools registered without an explicit
        // `RiskLevel` argument — they default to `Caution`,
        // which is the safe default, but the test ensures
        // every registered tool is at least *one of* Safe /
        // Caution / Dangerous (sanity check on the enum).
        // The probe list contains only unconditionally-
        // registered tools — those gated by env vars (e.g.
        // `code_execution` requiring a Python runtime,
        // homeassistant requiring `HASS_URL`) are excluded.
        let registry = registry_with_builtins();
        let sampled = [
            "terminal",
            "web_search",
            "web_extract",
            "url_safety",
            "clarify",
            "model_providers",
            "platforms",
            "security_guidance",
            "skills_list",
            "delegate_task",
            "send_message",
        ];
        for tool in sampled {
            assert!(
                registry.risk_level(tool).is_some(),
                "tool `{}` should be registered with a risk level",
                tool
            );
        }
    }

    #[test]
    fn risk_level_as_str_matches_snake_case_for_ws_payload() {
        // The WS event emitted by the dispatch gate uses
        // `risk.as_str()` for the `risk` field of the
        // `request_approval` event (see
        // `local_agent_service::decide_pre_tool_dispatch`).
        // The desktop client reads this as snake_case. If
        // the as_str() return values ever change, the
        // client-side `risk_level` filter will silently
        // mismatch and the badge in the approval card will
        // show the wrong color.
        assert_eq!(RiskLevel::Safe.as_str(), "safe");
        assert_eq!(RiskLevel::Caution.as_str(), "caution");
        assert_eq!(RiskLevel::Dangerous.as_str(), "dangerous");
    }

    #[test]
    fn get_definitions_includes_critical_tools() {
        // Regression guard: the four tools the user reported as
        // "missing" in the original regression MUST appear in
        // `get_definitions()` — this is what the LLM sees when
        // the agent loop calls `build_system_prompt`. Note that
        // the actual schema name for the code-execution tool is
        // `execute_code` (the *toolset tag* is `code_execution`);
        // the LLM sees the schema name, not the toolset tag.
        let registry = registry_with_builtins();
        let defs = registry.get_definitions();
        let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        for required in ["terminal", "list_files", "execute_code", "list_tools"] {
            assert!(
                names.iter().any(|n| n == required),
                "regression: get_definitions() does not include `{}`. \
                 The LLM will say 'I don't have {}' even though the \
                 tool is registered and ready to run.",
                required,
                required
            );
        }
    }

    #[test]
    #[ignore] // Run with `cargo test -- --ignored dump_all_tool_names --nocapture` to see the dump.
    fn dump_all_tool_names() {
        // Pure diagnostic: prints every schema name the registry
        // exposes via `get_definitions()`. Useful when updating
        // DOCUMENTED_SAFE_TOOLS or adding new tools — not a
        // regression test, kept ignored to avoid CI noise.
        let registry = registry_with_builtins();
        let defs = registry.get_definitions();
        let mut names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        names.sort();
        eprintln!("get_definitions() returned {} tools:", names.len());
        for n in &names {
            eprintln!("  {}", n);
        }
    }

    /// Diagnostic: the three bridge tools must be present **and enabled** in
    /// the registry, not merely injected as synthetic schemas by
    /// `assemble_tool_schemas`. When they diverge, the model sees
    /// `tool_describe` in its tools array, calls it, and the validator answers
    /// `Tool 'tool_describe' does not exist` — burning turns.
    #[test]
    #[ignore]
    fn dump_bridge_state() {
        let registry = registry_with_builtins();
        for name in ["tool_search", "tool_describe", "tool_call"] {
            match registry.list_tools().into_iter().find(|e| e.name == name) {
                Some(e) => eprintln!(
                    "{name}: present toolset={} enabled={}",
                    e.toolset, e.is_enabled
                ),
                None => eprintln!("{name}: MISSING from list_tools"),
            }
            eprintln!(
                "  in_definitions={}",
                registry.get_definitions().iter().any(|d| d.name == name)
            );
        }
    }

    #[test]
    fn essential_toolset_floor_aligns_with_runtime_registration() {
        //P1.8 : regression guard for a class of bugs
        // where the floor lists a toolset name that no tool is
        // actually tagged with. The symptom is silent: the floor
        // calls `set_toolset_enabled(<name>, true)`, the registry
        // finds zero matching tools, returns count=0, and the LLM
        // ends up without the supposedly-essential tool. The UI
        // toggle (`set_enabled_toolsets`) is the symmetric bug:
        // disabling a group name that no tool carries silently
        // flips nothing. The fix side of this test lives in
        // `tools::pdf` and
        // `essential::ESSENTIAL_TOOLSETS` — together they keep the
        // floor's expectations and the runtime registration in lockstep.
        let registry = registry_with_builtins();
        let runtime_toolsets: std::collections::HashSet<String> =
            registry.list_toolsets().into_iter().collect();

        let missing: Vec<&&str> = ESSENTIAL_TOOLSETS
            .iter()
            .filter(|name| !runtime_toolsets.contains(**name))
            .collect();
        assert!(
            missing.is_empty(),
            "ESSENTIAL_TOOLSETS references toolsets that no tool \
             is tagged with: {:?}. `set_toolset_enabled` for these \
             names is a silent no-op — the floor's whole point is \
             to *re-assert* a tool that's already wired, so a \
             missing tool means the LLM silently loses the tool \
             when the user whitelists only a subset. Add a \
             `register(...)` call that tags a tool with `<name>`, \
             or drop the entry from `ESSENTIAL_TOOLSETS`.",
            missing
        );
    }

    #[test]
    fn every_floor_group_toolset_is_a_real_group() {
        // Symmetric to `essential_toolset_floor_aligns_with_runtime_registration`:
        // the floor is anchored on *group* names (e.g. `"file"`),
        // not on per-tool names. If a future `register_with` call goes
        // back to tagging each tool with its own name, every file_*
        // tool would land in its own singleton toolset and the
        // floor would still pass the previous test (because the
        // per-tool names *do* exist in `list_toolsets()`), but the
        // floor would then flip only one tool per call. Pin the
        // expected group shape so that regression is loud.
        let registry = registry_with_builtins();
        let entries = registry.list_tools();
        let toolsets: std::collections::HashMap<String, usize> = {
            let mut map: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for e in &entries {
                *map.entry(e.toolset.clone()).or_insert(0) += 1;
            }
            map
        };
        // Grouped toolsets the floor relies on.
        let expected_groups: &[(&str, usize)] = &[
            ("file", 6),           // read/write/patch/search + list_files/edit_file
            ("pdf", 1),            // read_pdf (P1.8 split out of "file")
            ("homeassistant", 4),  // ha_* tools
        ];
        for (group, min_count) in expected_groups {
            let actual = toolsets.get(*group).copied().unwrap_or(0);
            assert!(
                actual >= *min_count,
                "toolset `{}` is supposed to be a *group* of at least \
                 {} tools (floor's whole point is one re-assert call \
                 covers them all), but only {} tools are tagged with it. \
                 A regression to per-tool tagging would make the floor \
                 miss every tool in the group except one.",
                group, min_count, actual
            );
        }
    }

    #[test]
    #[ignore] // Run with `cargo test -- --ignored list_registered_tools --nocapture` to see the dump.
    fn list_registered_tools() {
        // Diagnostic helper for understanding what's actually
        // in the registry after `register_builtin_tools`
        // runs. Not part of the regression suite; useful when
        // updating DOCUMENTED_SAFE_TOOLS or
        // DOCUMENTED_DANGEROUS_TOOLS.
        let registry = registry_with_builtins();
        let mut names: Vec<String> = Vec::new();
        // The inner state is private, so we probe a
        // reasonable set of likely names. If a name returns
        // `Some(risk)`, it's registered.
        let candidates = [
            "terminal",
            "process",
            "web_search",
            "web_extract",
            "url_safety",
            "code_execution",
            "skills_list",
            "skills_get",
            "skills_install",
            "clarify",
            "ha_list_entities",
            "ha_get_state",
            "ha_list_services",
            "ha_call_service",
            "platforms",
            "model_providers",
            "security_guidance",
            "send_message",
            "delegate_task",
            "process_list",
            "process_kill",
            "process_write",
            "memory_search",
            "memory_store",
            "evolution_status",
            "evolution_set",
            "autonomy_guidance",
            "self_restart",
            "image_generate",
            "gpt_image_generate",
            "video_generate",
            "video_status",
            "media_workflow",
            "compact_tool_chain",
        ];
        for name in candidates {
            if let Some(risk) = registry.risk_level(name) {
                names.push(format!("{}: {:?}", name, risk));
            }
        }
        names.sort();
        eprintln!("Registered tools ({} total):", names.len());
        for n in &names {
            eprintln!("  {}", n);
        }
    }
}

