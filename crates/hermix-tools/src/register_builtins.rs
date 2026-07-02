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
    );
    crate::tools::clarify::register(registry, clarify_backend);

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

    /// Parse the three browser sub-fields out of an already-read YAML
    /// string. Pure function (no IO) — every test exercises this
    /// path so we don't need to set `HERMIX_HOME` per case.
    fn from_yaml(text: &str) -> Self {
        let mut cfg = Self::default();
        let mut in_browser = false;
        let mut in_domains = false;
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let raw = lines[i];
            // Strip `#` comments before scanning. Don't touch `#`
            // inside quoted strings — the YAML we accept is too
            // simple to need that.
            let line = raw.split('#').next().unwrap_or("");
            if line.is_empty() {
                i += 1;
                continue;
            }
            let trimmed = line.trim_end();
            let trimmed_l = trimmed.trim_start();

            if !in_browser {
                if trimmed_l.starts_with("browser:") || trimmed_l.starts_with("browser :") {
                    in_browser = true;
                }
                i += 1;
                continue;
            }
            // Inside the `browser:` block. A new top-level key ends it.
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
                in_browser = false;
                in_domains = false;
                i += 1;
                continue;
            }

            if !in_domains {
                // Not in a list — try each recognised sub-key.
                let mut handled = true;
                if let Some(rest) = trimmed_l.strip_prefix("allowed_domains:") {
                    let rest = rest.trim();
                    // Inline list form: `allowed_domains: [a, b, c]`.
                    if let Some(inner) =
                        rest.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                    {
                        let domains = inner
                            .split(',')
                            .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\''))
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>();
                        if !domains.is_empty() {
                            cfg.allowed_domains = Some(domains);
                        }
                    }
                    in_domains = true;
                } else if let Some(rest) = trimmed_l.strip_prefix("cdp_port:") {
                    if let Ok(n) = rest.trim().parse::<u16>() {
                        cfg.cdp_port = Some(n);
                    }
                } else if let Some(rest) = trimmed_l.strip_prefix("headless:") {
                    cfg.headless = match rest.trim().to_ascii_lowercase().as_str() {
                        "true" | "yes" | "1" | "on" => Some(true),
                        "false" | "no" | "0" | "off" => Some(false),
                        _ => None,
                    };
                } else {
                    handled = false;
                }
                let _ = handled; // marker; kept for clarity of the if/else chain
                i += 1;
            } else {
                // Inside an `allowed_domains:` block-list.
                if let Some(rest) = trimmed_l.strip_prefix("- ") {
                    let s = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                    if !s.is_empty() {
                        cfg.allowed_domains
                            .get_or_insert_with(Vec::new)
                            .push(s.to_string());
                    }
                    i += 1;
                } else if trimmed_l.is_empty() {
                    // Blank line inside the list — keep in_domains.
                    i += 1;
                } else {
                    // Sibling sub-key — exit the list, re-process
                    // this line under the !in_domains branch by
                    // not advancing `i`. The next loop iteration
                    // sees in_domains=false and picks up
                    // cdp_port / headless / a fresh allowed_domains.
                    in_domains = false;
                }
            }
        }
        cfg
    }

    /// Read `$HERMIX_HOME/config.yaml` (or `$HERMES_HOME/config.yaml`
    /// as a legacy alias). Returns `None` when neither env var is
    /// set or the file is unreadable — startup should not panic.
    fn read_home_yaml() -> Option<String> {
        let home = std::env::var("HERMIX_HOME")
            .or_else(|_| std::env::var("HERMES_HOME"))
            .ok()?;
        let path = std::path::Path::new(&home).join("config.yaml");
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

    /// Live IO test — guarded by `HERMIX_HOME_LOCK` so we don't race
    /// with other tests that touch the env var.
    #[test]
    fn from_env_returns_default_when_home_missing() {
        let _guard = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("HERMIX_HOME").ok();
        let saved_hermes = std::env::var("HERMES_HOME").ok();
        std::env::remove_var("HERMIX_HOME");
        std::env::remove_var("HERMES_HOME");
        let cfg = BrowserConfig::from_env();
        assert_eq!(cfg, BrowserConfig::default());
        // Restore so subsequent tests see the original env.
        if let Some(v) = saved {
            std::env::set_var("HERMIX_HOME", v);
        }
        if let Some(v) = saved_hermes {
            std::env::set_var("HERMES_HOME", v);
        }
    }

    /// `HERMIX_HOME` is process-global env state; concurrent tests
    /// reading/writing it race. Acquire this lock for the entire
    /// duration of any test that touches `HERMIX_HOME`.
    static HERMIX_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Round-trip: write a YAML, point `HERMIX_HOME` at it, call
    /// `from_env()`, assert all three fields round-tripped. This
    /// is the only test that exercises the env→disk path; every
    /// other test goes through `from_yaml` so they don't have to
    /// juggle env vars.
    #[test]
    fn from_env_round_trip_through_disk() {
        use std::io::Write;
        use std::sync::atomic::AtomicU64;

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let _guard = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

        let saved = std::env::var("HERMIX_HOME").ok();
        std::env::set_var("HERMIX_HOME", &dir);
        let cfg = BrowserConfig::from_env();
        if let Some(v) = saved {
            std::env::set_var("HERMIX_HOME", v);
        } else {
            std::env::remove_var("HERMIX_HOME");
        }

        assert_eq!(cfg.allowed_domains, Some(vec!["a.com".to_string()]));
        assert_eq!(cfg.cdp_port, Some(9333));
        assert_eq!(cfg.headless, Some(true));
    }
}
