//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::collections::HashSet;
use std::sync::Arc;

use hermix_core::{SkillProvider, TerminalBackend};

use hermix_config::paths::hermix_home;

use crate::tools::clarify::ClarifyBackend;
use crate::ToolRegistry;

/// Register all built-in tool handlers into the given registry.
///
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
/// `clarify_backend` backs the `clarify` tool. Callers that want a
/// blocking user-prompt experience (e.g. the desktop app via
/// [`ChannelClarifyBackend`](crate::backends::clarify::ChannelClarifyBackend))
/// should pass it here; otherwise the default [`SignalClarifyBackend`](crate::backends::clarify::SignalClarifyBackend)
/// returns a JSON envelope without waiting.
///
/// Backends that depend on environment variables (e.g. `OPENAI_API_KEY`,
/// `FAL_KEY`, `HASS_URL`) are constructed eagerly; if the vars are absent
/// the tools will return a clear error at *runtime*, not at registration.
///
/// P1.4 (2026-07-02): the body is now an honest three-step pipeline
/// (build browser backend → register_all → reconcile whitelist + floor).
/// The three `browser_*_from_config` helpers were merged into
/// [`BrowserConfig::from_env`] (single IO instead of three) and the
/// per-step rationale moved into the function-level doc below so the
/// body itself reads top-to-bottom without inline commentary.
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    clarify_backend: Option<Arc<dyn ClarifyBackend>>,
    enabled_toolsets: Option<HashSet<String>>,
    todo_event_tx: Option<tokio::sync::broadcast::Sender<crate::tools::todo::TodoChange>>,
) {
    // 1. Browser backend. We construct it here (not inside
    //    `tools::browser::register`) because the allowlist / port /
    //    headless settings come from `config.yaml` and we don't want
    //    the browser module to reach across the crate for config
    //    helpers. The `SessionedBrowserBackend` wrapper hands each
    //    `tools::browser::register` handler an indirection that
    //    dispatches by `session_id`, so group-chat agents in
    //    different sessions never share CDP state.
    let browser_cfg = BrowserConfig::from_env();
    let browser_registry = std::sync::Arc::new(
        crate::backends::browser::BrowserRegistry::new()
            .with_allowed_domains(browser_cfg.allowed_domains)
            .with_cdp_port(browser_cfg.cdp_port)
            .with_headless(browser_cfg.headless),
    );
    let browser_backend: Arc<dyn crate::tools::browser::BrowserBackend> =
        std::sync::Arc::new(crate::backends::browser::SessionedBrowserBackend::new(
            browser_registry,
        ));

    // 2. Clarify backend. Caller-provided (desktop uses
    //    `ChannelClarifyBackend` for prompt-the-user) wins over the
    //    signal-based default. The default is used by tests and CLI
    //    callers that don't need a UI prompt.
    let clarify_backend: Arc<dyn ClarifyBackend> = match clarify_backend {
        Some(b) => b,
        None => Arc::new(crate::backends::clarify::SignalClarifyBackend::new()),
    };

    // 3. Wire everything into the registry via the per-tool
    //    `register(...)` functions. Then re-register `clarify` on
    //    top with the caller-provided backend so the desktop's
    //    blocking prompt overrides the signal default that
    //    `register_all` just installed.
    crate::tools::register_all(
        registry,
        terminal_backend,
        skill_provider,
        browser_backend,
        todo_event_tx,
    );
    crate::tools::clarify::register(registry, clarify_backend);
    // Mark interactive tools that must not run in parallel with others.
    registry.mark_interactive("clarify");
    registry.mark_interactive("delegate_task");

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());

    // 4. Whitelist post-pass. `tools::register_all` registers every
    //    tool unconditionally; this flips the ones the user did not
    //    whitelist back off, restoring the Settings UI's runtime
    //    toggle without re-registering. Mirrored wildcard (`"all"` /
    //    `"*"`) keeps "user wants everything on" working.
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

    // 5. Essential toolset floor. Re-asserts `terminal` / `file` /
    //    `code_execution` / `web` / `memory` / `pdf` / `core` /
    //    `security` AFTER the whitelist above. A hand-edited
    //    `enabled_toolsets.json` containing only `["file", "web"]`
    //    would otherwise leave the LLM answering "I don't have
    //    terminal". See `essential::ensure_essentials_enabled` for
    //    the full rationale.
    let reasserted = crate::essential::ensure_essentials_enabled(registry);
    if reasserted > 0 {
        tracing::info!(
            entries = reasserted,
            toolset = ?crate::essential::ESSENTIAL_TOOLSETS,
            "startup: re-enabled essential toolset floor (whitelist did not contain it)"
        );
    }
}

// `hermes_data_dir` / `dirs_home` were deleted: they bypassed
// `hermix_config::paths::hermix_home()` and ignored `HERMIX_PORTABLE_DATA_DIR`,
// which caused session_search to write to a different DB than the rest of
// the app in portable-desktop mode. Use `paths::sessions_db_path()` instead.
//
// `BrowserConfig::read_home_yaml` had the same P0: it read `$HERMIX_HOME` /
// `$HERMES_HOME` directly via `std::env::var` instead of going through
// `hermix_home()`. Fixed in P1.5 — portable-desktop launchers that set
// `HERMIX_PORTABLE_DATA_DIR` now resolve to the right config.yaml, and the
// legacy `$HERMES_HOME` alias (used by the original NousResearch agent)
// cannot silently clobber a developer's browser config.

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
            unimplemented!()
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
            unimplemented!()
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
        );
        registry
    }

    /// Documented-Safe tools — read-only by design and
    /// unconditionally registered (no env-var or feature
    /// gates). If any of these regress to `Caution`/`Dangerous`,
    /// the dispatch gate will start firing unnecessary
    /// approval cards.
    ///
    /// When adding a new Safe tool: add it here. When removing
    /// one: remove the line and document the rationale.
    ///
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
    #[ignore] // Run with `cargo test -- --ignored list_registered_tools --nocapture` to see the dump.
    fn list_registered_tools_full() {
        // Probe-driven: dump every schema name the registry actually
        // exposes via `get_definitions()` (this is what the LLM sees
        // when the agent loop calls `build_system_prompt`). The
        // `list_registered_tools` test above only checks a hard-coded
        // candidate list, which is why a "LLM says it has no
        // terminal" regression could slip past it.
        let registry = registry_with_builtins();
        let defs = registry.get_definitions();
        let mut names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        names.sort();
        eprintln!("get_definitions() returned {} tools:", names.len());
        for n in &names {
            eprintln!("  {}", n);
        }
        // Hard-pin the four tools the user reported as "missing" in
        // the 2026-06-14 regression. Note that the actual schema name
        // for the code-execution tool is `execute_code` (the *toolset
        // tag* is `code_execution`); the LLM sees the schema name.
        for required in ["terminal", "list_files", "execute_code", "list_tools"] {
            assert!(
                names.iter().any(|n| n == required),
                "regression: get_definitions() does not include `{}`",
                required
            );
        }
    }

    #[test]
    fn essential_toolset_floor_aligns_with_runtime_registration() {
        // P1.8 (2026-07-02): regression guard for a class of bugs
        // where the floor lists a toolset name that no tool is
        // actually tagged with. The symptom is silent: the floor
        // calls `set_toolset_enabled(<name>, true)`, the registry
        // finds zero matching tools, returns count=0, and the LLM
        // ends up without the supposedly-essential tool. The UI
        // toggle (`set_enabled_toolsets`) is the symmetric bug:
        // a user who disables `"browser"` thinks they turned off
        // every browser_* tool, but the registry only flips the
        // (non-existent) `"browser"` tag and leaves them all on.
        //
        // The fix side of this test lives in `tools::browser`,
        // `tools::cua`, `tools::pdf`, `tools::office_cli`, and
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
        // the floor is anchored on *group* names (`"browser"`, `"cua"`),
        // not on per-tool names. If a future `register_with` call goes
        // back to tagging each tool with its own name, every browser_*
        // / cua_* tool would land in its own singleton toolset and the
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
            ("browser", 7),        // 7 browser_* tools share one group
            ("cua", 14),           // 14 cua_* tools share one group
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
            "browser",
            "vision",
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

/// Read `browser.allowed_domains` / `browser.cdp_port` / `browser.headless`
/// from `~/.hermix/config.yaml`.
///
/// Hand-rolled YAML scan — we don't want a full YAML dep here for
/// three fields. The expected shape is:
/// ```yaml
/// browser:
///   allowed_domains:
///     - example.com
///     - "*.trusted.io"
///   cdp_port: 9223
///   headless: true
/// ```
///
/// P1.4 (2026-07-02): the previous design had three separate
/// `browser_*_from_config()` helpers, each of which independently
/// re-`read_to_string`'d the file and re-scanned for its own key.
/// That was three filesystem reads per `register_builtin_tools`
/// call, plus 150 lines of near-duplicate code. [`BrowserConfig`]
/// now reads the file once and populates every field in a single
/// pass; the call site collapses to a single `BrowserConfig::from_env()`
/// expression.
#[derive(Default, Debug, PartialEq, Eq)]
struct BrowserConfig {
    /// `Some(list)` restricts navigation. `None` = no allowlist (the
    /// SSRF guard still rejects private addresses). An empty `Some(vec![])`
    /// would mean "no navigation allowed" — we collapse to `None` here
    /// to keep the runtime semantics intuitive.
    allowed_domains: Option<Vec<String>>,
    /// CDP port to use for browser bootstrap. `None` falls back to
    /// `cdp_bootstrap::DEFAULT_CDP_PORT` (9222) or `HERMIX_BROWSER_CDP_PORT`.
    cdp_port: Option<u16>,
    /// Override the `HERMIX_BROWSER_HEADLESS` env-var fallback. `None`
    /// = consult the env var next.
    headless: Option<bool>,
}

impl BrowserConfig {
    /// Read the user's `config.yaml` from `$HERMIX_HOME` (with the
    /// legacy `$HERMES_HOME` alias) and extract every browser
    /// sub-field. Returns [`Default::default`] (all `None`) when the
    /// file is missing or unreadable — startup should not fail
    /// because the operator has not configured browser details yet.
    fn from_env() -> Self {
        match Self::read_home_yaml() {
            Some(text) => Self::from_yaml(&text),
            None => Self::default(),
        }
    }

    /// Test-only entry point: read `config.yaml` from an explicit
    /// directory. Lets unit tests exercise the disk round-trip
    /// without mutating the process-global `$HERMIX_HOME` env var.
    ///
    /// Why a parallel API: `from_env` reads `HERMIX_HOME` via
    /// `hermix_config::paths::hermix_home()`, which re-reads the env
    /// var on every call. Concurrent tests that mutate
    /// `$HERMIX_HOME` to point at their own temp dir race even when
    /// they hold `HERMIX_HOME_LOCK` — the lock only serialises the
    /// test thread, but `hermix_home()` runs on whatever thread the
    /// caller is on and sees whatever value is in the env at that
    /// instant. Passing an explicit `home` argument removes the env
    /// var from the loop entirely.
    #[cfg(test)]
    fn from_home(home: &std::path::Path) -> Self {
        let path = home.join("config.yaml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_yaml(&text),
            Err(_) => Self::default(),
        }
    }

    /// Parse the three browser sub-fields out of an already-read YAML
    /// string. Pure function (no IO) — every test exercises this
    /// path so we don't need to set `HERMIX_HOME` per case.
    ///
    /// P1.8 (2026-07-02): rewritten via `serde_yaml` so we get a
    /// declarative schema instead of a 90-line line scanner. The
    /// shape is exactly the same as before — `Some(vec)` for a
    /// populated list, `None` for "absent" — so callers (and the
    /// 14 test cases below) keep working unchanged.
    fn from_yaml(text: &str) -> Self {
        // Accept the same loose truthy/falsy spellings the previous
        // hand-rolled parser accepted (true/yes/1/on vs false/no/0/off).
        // We parse integers as 0/1 too — `serde_yaml` unmarks bare `1`
        // as i64 by default, so we accept it via the `Int` variant.
        #[derive(serde::Deserialize, Default)]
        #[serde(untagged)]
        enum HeadlessLoose {
            Bool(bool),
            Int(i64),
            Str(HeadlessStr),
            #[default]
            Other,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum HeadlessStr {
            True,
            Yes,
            On,
            False,
            No,
            Off,
        }

        impl HeadlessLoose {
            fn resolve(self) -> Option<bool> {
                match self {
                    HeadlessLoose::Bool(b) => Some(b),
                    HeadlessLoose::Int(n) => match n {
                        1 => Some(true),
                        0 => Some(false),
                        _ => None,
                    },
                    HeadlessLoose::Str(s) => match s {
                        HeadlessStr::True | HeadlessStr::Yes | HeadlessStr::On => Some(true),
                        HeadlessStr::False | HeadlessStr::No | HeadlessStr::Off => Some(false),
                    },
                    HeadlessLoose::Other => None,
                }
            }
        }

        #[derive(serde::Deserialize, Default)]
        struct BrowserYaml {
            #[serde(default)]
            allowed_domains: Option<Vec<String>>,
            #[serde(default)]
            cdp_port: Option<u16>,
            #[serde(default)]
            headless: Option<HeadlessLoose>,
        }

        #[derive(serde::Deserialize, Default)]
        struct RootYaml {
            #[serde(default)]
            browser: Option<BrowserYaml>,
        }

        let root: RootYaml = match serde_yaml::from_str(text) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let Some(b) = root.browser else {
            return Self::default();
        };

        Self {
            allowed_domains: b.allowed_domains.and_then(|v| {
                let cleaned: Vec<String> = v
                    .into_iter()
                    .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                }
            }),
            cdp_port: b.cdp_port,
            headless: b.headless.and_then(HeadlessLoose::resolve),
        }
    }

    /// Read the canonical `$HERMIX_HOME/config.yaml` (resolved via
    /// [`hermix_home`], which honors `HERMIX_PORTABLE_DATA_DIR` for
    /// the Tauri portable-desktop launcher).
    ///
    /// **Deliberate non-features** vs. the previous implementation:
    ///
    /// * **No `$HERMES_HOME` fallback.** The original NousResearch
    ///   `hermes-agent` uses the same env var name; silently picking
    ///   it up would let two unrelated projects fight over one data
    ///   dir. Users running the old agent should set `HERMIX_HOME`
    ///   explicitly or run with `HERMIX_MIGRATE_FROM_HERMES=1`.
    /// * **No raw `std::env::var("HERMIX_HOME")` lookup.** Going
    ///   through `hermix_home()` ensures the portable launcher and
    ///   the standard install resolve to the same file (this is the
    ///   same P0 fix that already cleaned up `session_search`).
    ///
    /// Returns `None` when the home is empty or `config.yaml` is
    /// unreadable — startup should not panic on a missing config.
    fn read_home_yaml() -> Option<String> {
        let home = hermix_home();
        let path = home.join("config.yaml");
        std::fs::read_to_string(&path).ok()
    }
}

#[cfg(test)]
mod browser_config_tests {
    use super::*;

    #[test]
    fn parses_all_three_fields_in_one_pass() {
        // The P1.4 win: a single YAML block, a single read, all
        // three fields populated. The previous code did three reads.
        let cfg = BrowserConfig::from_yaml(
            "browser:\n  allowed_domains:\n    - example.com\n    - \"*.trusted.io\"\n  cdp_port: 9223\n  headless: true\n",
        );
        assert_eq!(
            cfg.allowed_domains,
            Some(vec!["example.com".to_string(), "*.trusted.io".to_string()])
        );
        assert_eq!(cfg.cdp_port, Some(9223));
        assert_eq!(cfg.headless, Some(true));
    }

    #[test]
    fn returns_default_when_browser_block_absent() {
        let cfg = BrowserConfig::from_yaml("other_key: 1\n");
        assert_eq!(cfg, BrowserConfig::default());
    }

    #[test]
    fn allowed_domains_inline_array_syntax() {
        let cfg = BrowserConfig::from_yaml("browser:\n  allowed_domains: [a.com, b.com]\n");
        assert_eq!(
            cfg.allowed_domains,
            Some(vec!["a.com".to_string(), "b.com".to_string()])
        );
    }

    #[test]
    fn cdp_port_missing_yields_none() {
        let cfg = BrowserConfig::from_yaml("browser:\n  allowed_domains: [a.com]\n");
        assert_eq!(cfg.cdp_port, None);
    }

    #[test]
    fn cdp_port_invalid_value_yields_none_not_panic() {
        let cfg = BrowserConfig::from_yaml("browser:\n  cdp_port: not_a_number\n");
        assert_eq!(cfg.cdp_port, None);
    }

    #[test]
    fn headless_accepts_truthy_and_falsy_aliases() {
        for (v, expected) in [
            ("true", Some(true)),
            ("True", Some(true)),
            ("yes", Some(true)),
            ("1", Some(true)),
            ("on", Some(true)),
            ("false", Some(false)),
            ("False", Some(false)),
            ("no", Some(false)),
            ("0", Some(false)),
            ("off", Some(false)),
        ] {
            let yaml = format!("browser:\n  headless: {v}\n");
            let cfg = BrowserConfig::from_yaml(&yaml);
            assert_eq!(cfg.headless, expected, "headless={v}");
        }
    }

    #[test]
    fn headless_unrecognised_value_yields_none() {
        let cfg = BrowserConfig::from_yaml("browser:\n  headless: maybe\n");
        assert_eq!(cfg.headless, None);
    }

    #[test]
    fn allowed_domains_quoted_strings_strip_quotes() {
        let cfg =
            BrowserConfig::from_yaml("browser:\n  allowed_domains:\n    - \"quoted.example\"\n");
        assert_eq!(
            cfg.allowed_domains,
            Some(vec!["quoted.example".to_string()])
        );
    }

    #[test]
    fn comments_are_stripped_before_parsing() {
        let cfg = BrowserConfig::from_yaml(
            "# top-level comment\nbrowser:\n  # block comment\n  cdp_port: 9333\n  headless: false\n",
        );
        assert_eq!(cfg.cdp_port, Some(9333));
        assert_eq!(cfg.headless, Some(false));
    }

    #[test]
    fn sibling_top_level_key_ends_browser_block() {
        // After `logging:` the scanner must not pick up
        // `level:` as a browser sub-field.
        let cfg = BrowserConfig::from_yaml(
            "browser:\n  cdp_port: 9223\nlogging:\n  level: info\n",
        );
        assert_eq!(cfg.cdp_port, Some(9223));
    }

    /// Live IO test for the disk round-trip path: when the
    /// `<home>/config.yaml` file is missing, [`BrowserConfig::from_home`]
    /// must return `default()` rather than panic. This exercises the
    /// same code path as `from_env` (`read_to_string` → `from_yaml`),
    /// just with the home dir passed explicitly so we don't have to
    /// touch the process-global `$HERMIX_HOME` env var.
    #[test]
    fn from_home_returns_default_when_config_missing() {
        use std::sync::atomic::AtomicU64;
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hermix-browser-config-empty-{}-{n}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Note: deliberately do NOT create config.yaml.

        let cfg = BrowserConfig::from_home(&dir);
        assert_eq!(cfg, BrowserConfig::default());
    }

    /// The "don't silently read `$HERMES_HOME`" semantics are owned
    /// by `hermix_config::paths::hermix_home()` and exercised there
    /// in `tests::ignores_legacy_hermes_home`. They are NOT
    /// duplicated here — `BrowserConfig::from_env` is now just
    /// `read_home_yaml + from_yaml`, and we want the YAML layer to
    /// stay testable without touching process-global env state.

    /// Round-trip: write a YAML to `<home>/config.yaml`, point
    /// [`BrowserConfig::from_home`] at the directory, assert all
    /// three fields round-tripped. This exercises the same
    /// `read_to_string → from_yaml` path that `from_env` walks, but
    /// without mutating `$HERMIX_HOME` so concurrent tests can't
    /// race us through `hermix_home()`.
    #[test]
    fn from_home_round_trip_through_disk() {
        use std::io::Write;
        use std::sync::atomic::AtomicU64;

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hermix-browser-config-{}-{n}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(
                b"browser:\n  allowed_domains:\n    - a.com\n  cdp_port: 9333\n  headless: true\n",
            )
            .unwrap();
        }

        let cfg = BrowserConfig::from_home(&dir);

        assert_eq!(cfg.allowed_domains, Some(vec!["a.com".to_string()]));
        assert_eq!(cfg.cdp_port, Some(9333));
        assert_eq!(cfg.headless, Some(true));
    }
}
