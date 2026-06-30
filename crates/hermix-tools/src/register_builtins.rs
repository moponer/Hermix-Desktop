//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::collections::HashSet;
use std::sync::Arc;

use hermix_core::{traits::RiskLevel, SkillProvider, TerminalBackend, ToolHandler};

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
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    clarify_backend: Option<Arc<dyn ClarifyBackend>>,
    enabled_toolsets: Option<HashSet<String>>,
) {
    /// `enabled_toolsets: Some({...})` → only register tools whose
    /// toolset tag is in the set. `None` → register everything
    /// (back-compat). Special tags `"all"` and `"*"` mean "register
    /// every tool regardless of group".
    fn is_enabled(enabled: &Option<HashSet<String>>, toolset: &str) -> bool {
        match enabled {
            None => true,
            Some(set) => set.contains("all") || set.contains("*") || set.contains(toolset),
        }
    }

    /// Default-`Caution` registration. Use `reg_risk` for Safe / Dangerous
    /// tools.
    fn reg(
        enabled_toolsets: &Option<HashSet<String>>,
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
    ) {
        reg_risk(
            enabled_toolsets,
            registry,
            toolset,
            handler,
            emoji,
            env_deps,
            RiskLevel::Caution,
        );
    }

    /// Registration with an explicit [`RiskLevel`]. The pre-execution approval
    /// hook uses this to decide whether to prompt the user in
    /// `PermissionLevel::Default` mode.
    fn reg_risk(
        enabled_toolsets: &Option<HashSet<String>>,
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
        risk_level: RiskLevel,
    ) {
        // Toolset filter: we ALWAYS register the tool now (the
        // handler closure is cheap to construct and we need the
        // entry in the registry so the Settings UI can flip it
        // back on later). The actual "hide from LLM" decision is
        // applied as a post-pass at the bottom of
        // `register_builtin_tools`, which walks
        // `registry.list_toolsets()` and flips `is_enabled=false`
        // for every entry whose toolset tag is not in the
        // user's enabled set. This keeps the original
        // `enabled_toolsets` parameter's *effective* behaviour
        // (LLM only sees the whitelisted toolsets) while
        // restoring the runtime-toggle capability that the
        // Settings UI needs.
        if !is_enabled(enabled_toolsets, toolset) {
            let schema = handler.schema();
            tracing::debug!(
                tool = %schema.name,
                toolset = %toolset,
                "startup: toolset not in enabled set, will mark is_enabled=false post-pass"
            );
        }
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register_with_risk(
            name,
            toolset,
            schema,
            handler,
            Arc::new(|| true),
            env_deps,
            true,
            desc,
            emoji,
            None,
            risk_level,
            /* override_existing = */ false,
        );
    }

    // -- Web tools -----------------------------------------------------------
    // Web search: keyless Baidu HTML scraper (no API key, works out of the box).
    reg_risk(
        &enabled_toolsets,
        registry,
        "web",
        Arc::new(crate::tools::web::WebSearchHandler::new(Box::new(
            crate::backends::web::BaiduHtmlSearchBackend::new(),
        ))),
        "🔍",
        vec![],
        RiskLevel::Safe,
    );
    // Web extract: keyless reqwest HTML fetch (no Firecrawl, no API key).
    reg_risk(
        &enabled_toolsets,
        registry,
        "web",
        Arc::new(crate::tools::web::WebExtractHandler::new(Box::new(
            crate::backends::web::SimpleExtractBackend::new(),
        ))),
        "📄",
        vec![],
        RiskLevel::Safe,
    );

    // -- Managed Tool Gateway -----------------------------------------------
    // Routes tool calls through an HTTP gateway (e.g. Nous managed tools).
    // Only functional when HERMIX_MANAGED_TOOL_GATEWAY_URL or vendor config
    // is set; returns a helpful error otherwise.
    // -- Terminal ------------------------------------------------------------
    // Registered as `Caution`, not `Dangerous`. The tool itself can run
    // anything from `ls` to `rm -rf`, so the tool-level *capability* is
    // dangerous, but the per-call risk is decided by the inner
    // `ApprovalManager` (see `hermix_tools::approval`) which already
    // denies `rm -rf /` and similar outright and tags `sudo` / `apt
    // install` as `RequiresConfirmation`. Marking the tool `Dangerous`
    // makes `local_agent_service::on_pre_tool_dispatch` pre-prompt
    // *every* terminal call — including `ls -la` and `echo hello`,
    // which is a false-positive the user has explicitly called out.
    // `Caution` keeps the policy intent ("can affect the system, the
    // command shape decides whether to actually interrupt") aligned
    // with the gate's own design note.
    reg_risk(
        &enabled_toolsets,
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::TerminalHandler::new(
            terminal_backend.clone(),
            registry.terminal_workspace_handle(),
        )),
        "💻",
        vec![],
        RiskLevel::Caution,
    );

    // -- File tools ----------------------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::file::ReadFileHandler::new(
            terminal_backend.clone(),
        )),
        "📖",
        vec![],
        RiskLevel::Safe,
    );
    reg(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::file::WriteFileHandler::new(
            terminal_backend.clone(),
        )),
        "✏️",
        vec![],
    );
    reg(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::file::PatchHandler::new(Arc::new(
            crate::backends::file::LocalPatchBackend::new(),
        ))),
        "🩹",
        vec![],
    );
    reg(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::file::SearchFilesHandler::new(Arc::new(
            crate::backends::file::LocalSearchBackend::new(),
        ))),
        "🔎",
        vec![],
    );
    // -- Aliases for search_files / patch ----------------------------------
    // The LLM is far more likely to call `list_files` / `edit_file`
    // (the natural-language names) than the canonical
    // `search_files` / `patch`. Register thin wrappers with the same
    // backends so neither call returns "Unknown tool".
    reg(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::aliases::ListFilesHandler::new(Arc::new(
            crate::backends::file::LocalSearchBackend::new(),
        ))),
        "📂",
        vec![],
    );
    reg(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::aliases::EditFileHandler::new(Arc::new(
            crate::backends::file::LocalPatchBackend::new(),
        ))),
        "✏️",
        vec![],
    );
    // -- Introspection -----------------------------------------------------
    // Always-on meta-tool that dumps every registered schema so the
    // LLM can recover from "Unknown tool" failures and discover
    // alias names at runtime. Safe to call repeatedly.
    reg_risk(
        &enabled_toolsets,
        registry,
        "core",
        Arc::new(crate::tools::aliases::ListToolsHandler::new(
            Arc::new(registry.clone()),
        )),
        "🧰",
        vec![],
        RiskLevel::Safe,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "file",
        Arc::new(crate::tools::pdf::ReadPdfHandler::new()),
        "📕",
        vec![],
        RiskLevel::Safe,
    );

    // vision tool migrated to tools/vision.rs::register (P1.2, 2026-06-30)

    // -- Image generation (fal.ai) -------------------------------------------
    {
        let backend = crate::backends::image_gen::FalImageGenBackend::from_env()
            .unwrap_or_else(|_| crate::backends::image_gen::FalImageGenBackend::new(String::new()));
        reg(
                    &enabled_toolsets,
                    registry,
            "image_gen",
            Arc::new(crate::tools::image_gen::ImageGenerateHandler::new(
                Arc::new(backend),
            )),
            "🎨",
            vec!["FAL_KEY".into()],
        );
    }

    // -- GPT Image (OpenAI gpt-image-1) -------------------------------------
    {
        let backend = crate::backends::gpt_image::OpenAiGptImageBackend::from_env_or_managed()
            .unwrap_or_else(|_| {
                crate::backends::gpt_image::OpenAiGptImageBackend::new(
                    String::new(),
                    "https://api.openai.com/v1".into(),
                )
            });
        reg(
                    &enabled_toolsets,
                    registry,
            "image_gen",
            Arc::new(crate::tools::gpt_image::GptImageHandler::new(Arc::new(
                backend,
            ))),
            "🖼️",
            vec!["OPENAI_API_KEY".into()],
        );
    }

    // -- Video generation (SeedDance2) ---------------------------------------
    {
        let backend =
            crate::backends::video_gen::SeedDance2Backend::from_env().unwrap_or_else(|_| {
                crate::backends::video_gen::SeedDance2Backend::new(
                    String::new(),
                    "https://api.seeddance.com/v2".into(),
                )
            });
        let backend = Arc::new(backend);
        reg(
                    &enabled_toolsets,
                    registry,
            "video",
            Arc::new(crate::tools::video_gen::VideoGenerateHandler::new(
                backend.clone(),
            )),
            "🎬",
            vec!["SEEDDANCE_API_KEY".into()],
        );
    }

    // -- Audio generation (music, SFX, voice cloning) ------------------------

    // -- Media workflow orchestrator -----------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "media",
        Arc::new(crate::tools::media_workflow::MediaWorkflowHandler::new(
            Arc::new(crate::tools::media_workflow::PlanningWorkflowBackend::new()),
        )),
        "🎞️",
        vec![],
    );

    // -- Skills (3 tools) ----------------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillsListHandler::new(
            skill_provider.clone(),
        )),
        "📚",
        vec![],
        RiskLevel::Safe,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillViewHandler::new(
            skill_provider.clone(),
        )),
        "📖",
        vec![],
        RiskLevel::Safe,
    );
    reg(
        &enabled_toolsets,
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillManageHandler::new(
            skill_provider,
        )),
        "⚙️",
        vec![],
    );

    // -- compact_tool_chain (P0-2) ------------------------------------------
    // LLM-facing tool that collapses N tool calls into one
    // tool_result entry — the main lever for keeping long
    // sessions under the context budget. Lives in its own
    // `compact` toolset so it can be toggled off separately
    // from the regular skill management tools if the user
    // wants raw tool_use calls back.
    //
    // We hand the chain its own cheap `Arc<ToolRegistry>` clone
    // (the inner state is `Arc<Mutex<...>>` so this is just an
    // atomic refcount bump, not a deep copy). The chain can
    // then dispatch any tool already registered, including ones
    // registered *after* this call — the registry's `Clone` is
    // a view, not a snapshot.
    reg_risk(
        &enabled_toolsets,
        registry,
        "compact",
        Arc::new(crate::tools::tool_chain::CompactToolChainHandler::new(
            Arc::new(registry.clone()),
        )),
        "🔗",
        vec![],
        RiskLevel::Safe,
    );

    // -- Memory --------------------------------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "memory",
        Arc::new(crate::tools::memory::MemoryHandler::new(Arc::new(
            crate::backends::memory::FileMemoryBackend::new(),
        ))),
        "🧠",
        vec![],
    );
    // Memory frozen-snapshot mode: log that the snapshot
    // machinery is engaged so the operator can grep for
    // `Memory: snapshot=` and confirm prefix-cache protection
    // is active. The actual snapshot read happens lazily on
    // the first `MemoryBackend::current_snapshot()` call (the
    // agent loop drives that, not us here).
    tracing::info!(
        "Memory: frozen-snapshot mode active (MEMORY.md/USER.md read once, \
         pinned for the session; add/replace/remove write to disk but do not \
         mutate the snapshot — this protects the LLM prefix cache). \
         Call `MemoryBackend::refresh_snapshot()` to re-pin from disk."
    );

    // -- Disk cleanup --------------------------------------------------------
    crate::tools::disk_cleanup::register(registry);

    // -- Computer use (desktop automation) -----------------------------------
    // Dangerous because every action operates on the live user
    // desktop (clicks, keystrokes, clipboard). The pre-tool-
    // dispatch layer in `LocalAgentService::decide_pre_tool_dispatch`
    // requires explicit user approval in `PermissionLevel::Default`.

    // -- CUA (self-hosted Computer-Use Automation, Phase 1) -----------------
    // Five-tool toolset backed by the `hermix-cua` crate (BitBlt
    // capture + `SendInput` injection, no third-party runtime).
    // Each tool is independently Dangerous because every call
    // either exposes the user's desktop (screenshot → privacy)
    // or mutates input state (click / type / press_key →
    // destructive if mis-targeted). The pre-tool-dispatch layer
    // prompts the user in `PermissionLevel::Default`.
    //
    // The existing `computer_use` handler above stays registered
    // — it is the broader 10-action surface (clipboard, scroll,
    // multi-display) that lives on the `enigo` / `xcap` /
    // `arboard` stack. The five CUA tools are the lower-cost
    // sibling toolset for tasks that only need the Phase 1
    // primitives; the dispatch gate decides which surface an
    // agent call resolves to based on the tool name.
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaScreenshotHandler::new()),
        "📸",
        vec![],
        RiskLevel::Dangerous,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaScreenInfoHandler::new()),
        "🖥️",
        vec![],
        RiskLevel::Dangerous,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaLeftClickHandler::new()),
        "🖱️",
        vec![],
        RiskLevel::Dangerous,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaTypeTextHandler::new()),
        "⌨️",
        vec![],
        RiskLevel::Dangerous,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaPressKeyHandler::new()),
        "🔘",
        vec![],
        RiskLevel::Dangerous,
    );

    // -- Phase 2-B: UIA element recognition (Win32 layer) -------------------
    // Three new handlers in the same `cua` toolset so the LLM sees
    // one cohesive desktop-automation toolset of 8 tools. Risk is
    // Dangerous for all three — `cua_click_window` issues a real
    // mouse click via SendInput, so it MUST go through the same
    // pre-tool approval card as the Phase 1 set.

    // -- CUA Phase 2-C: UIA element-tree handlers -------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaUiTreeHandler::new()),
        "🌳",
        vec![],
        RiskLevel::Dangerous,
    );
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaUiClickHandler::new()),
        "👆",
        vec![],
        RiskLevel::Dangerous,
    );

    // -- CUA Phase 2-D: OCR -----------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "computer_use",
        Arc::new(crate::tools::cua::CuaOcrRecognizeHandler::new()),
        "🔤",
        vec![],
        RiskLevel::Safe, // read-only, no mouse/keyboard injection
    );

    // -- Hermes achievements -------------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "hermix_achievements",
        Arc::new(crate::tools::hermix_achievements::HermixAchievementsHandler::new()),
        "🏆",
        vec![],
        RiskLevel::Safe,
    );

    // -- Session search ------------------------------------------------------
    {
        let db_path = hermix_config::paths::sessions_db_path();
        if let Ok(backend) = crate::backends::session_search::SqliteSessionSearchBackend::new(
            &db_path.to_string_lossy(),
        ) {
            reg_risk(
                        &enabled_toolsets,
                        registry,
                "session_search",
                Arc::new(crate::tools::session_search::SessionSearchHandler::new(
                    Arc::new(backend),
                )),
                "🔍",
                vec![],
                RiskLevel::Safe,
            );
        } else {
            tracing::warn!("Failed to initialise session search DB; skipping session_search tool");
        }
    }

    // -- Todo ----------------------------------------------------------------
    {
        let todo_path = hermix_config::paths::hermix_home().join("todos.json");
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "todo",
            Arc::new(crate::tools::todo::TodoHandler::new(Arc::new(
                crate::backends::todo::FileTodoBackend::new(todo_path),
            ))),
            "📋",
            vec![],
            RiskLevel::Safe,
        );
    }

    // -- Kanban --------------------------------------------------------------
    {
        let kanban_path = hermix_config::paths::hermix_home().join("kanban.json");
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "kanban",
            Arc::new(crate::tools::kanban::KanbanHandler::new(Arc::new(
                crate::tools::kanban::FileKanbanBackend::new(kanban_path),
            ))),
            "📌",
            vec![],
            RiskLevel::Safe,
        );
    }

    // -- Model Providers -----------------------------------------------------
    {
        let providers_path = hermix_config::paths::hermix_home().join("model_providers.json");
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "model_providers",
            Arc::new(crate::tools::model_providers::ModelProvidersHandler::new(
                Arc::new(crate::tools::model_providers::FileModelProvidersBackend::new(
                    providers_path,
                )),
            )),
            "🔧",
            vec![],
            RiskLevel::Safe,
        );
    }

    // -- Platforms -----------------------------------------------------------
    // platforms migrated to tools/platforms.rs::register (P1.2, 2026-06-30)

    // -- Security Guidance ---------------------------------------------------
    // security_guidance migrated to tools/security_guidance.rs::register (P1.2, 2026-06-30)

    // -- Clarify -------------------------------------------------------------
    let clarify_backend: Arc<dyn ClarifyBackend> = match clarify_backend {
        Some(b) => b,
        None => Arc::new(crate::backends::clarify::SignalClarifyBackend::new()),
    };
    reg_risk(
        &enabled_toolsets,
        registry,
        "clarify",
        Arc::new(crate::tools::clarify::ClarifyHandler::new(clarify_backend)),
        "❓",
        vec![],
        RiskLevel::Safe,
    );

    // -- Code execution ------------------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "code_execution",
        Arc::new(crate::tools::code_execution::ExecuteCodeHandler::new(
            Arc::new(crate::backends::code_execution::LocalCodeExecutionBackend::default()),
        )),
        "🖥️",
        vec![],
        RiskLevel::Caution,
    );

    // -- Delegation ----------------------------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "delegation",
        Arc::new(crate::tools::delegation::DelegateTaskHandler::new(
            Arc::new(crate::backends::delegation::SignalDelegationBackend::new()),
        )),
        "🤝",
        vec![],
    );

    // -- Cronjob -------------------------------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "cronjob",
        Arc::new(crate::tools::cronjob::CronjobHandler::new(Arc::new(
            crate::backends::cronjob::SignalCronjobBackend::new(),
        ))),
        "⏰",
        vec![],
    );

    // -- Messaging -----------------------------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "messaging",
        Arc::new(crate::tools::messaging::SendMessageHandler::new(Arc::new(
            crate::backends::messaging::SignalMessagingBackend::new(),
        ))),
        "💬",
        vec![],
    );

    // -- Home Assistant (4 tools) --------------------------------------------
    {
        let ha_backend: Arc<dyn crate::tools::homeassistant::HomeAssistantBackend> =
            match crate::backends::homeassistant::HaRestBackend::from_env() {
                Ok(b) => Arc::new(b),
                Err(_) => Arc::new(crate::backends::homeassistant::HaRestBackend::new(
                    String::new(),
                    String::new(),
                )),
            };
        let deps = vec!["HASS_URL".into(), "HASS_TOKEN".into()];
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListEntitiesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
            RiskLevel::Safe,
        );
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaGetStateHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
            RiskLevel::Safe,
        );
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListServicesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
            RiskLevel::Safe,
        );
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaCallServiceHandler::new(
                ha_backend,
            )),
            "🏠",
            deps,
            RiskLevel::Dangerous,
        );
    }

    // -- TTS -----------------------------------------------------------------
    reg(
        &enabled_toolsets,
        registry,
        "tts",
        Arc::new(crate::tools::tts::TextToSpeechHandler::new(Arc::new(
            crate::backends::tts::MultiTtsBackend::new(),
        ))),
        "🔊",
        vec![],
    );

    // -- Browser (10 tools) --------------------------------------------------
    {
        // Phase 3: BrowserRegistry + SessionedBrowserBackend.
        // The registry maps `session_id -> Arc<AutoBrowserBackend>` so
        // group-chat agents in different sessions never share CDP state.
        // The SessionedBrowserBackend wrapper is what the handlers hold:
        // every tool call dispatches to the backend for the active
        // session. Domain allowlist is loaded from `config.yaml` so
        // operators can restrict where the LLM is allowed to navigate.
        let browser_registry = std::sync::Arc::new(
            crate::backends::browser::BrowserRegistry::new()
                .with_allowed_domains(browser_allowed_domains_from_config())
                .with_cdp_port(browser_cdp_port_from_config())
                .with_headless(browser_headless_from_config()),
        );
        let sessioned = std::sync::Arc::new(
            crate::backends::browser::SessionedBrowserBackend::new(browser_registry),
        );
        let browser_backend: Arc<dyn crate::tools::browser::BrowserBackend> = sessioned;
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(
                crate::tools::browser::BrowserNavigateHandler::new(browser_backend.clone())
                    .with_cdp_bootstrap(crate::tools::cdp_bootstrap::DEFAULT_CDP_PORT),
            ),
            "🌐",
            vec![],
            RiskLevel::Safe,
        );
        reg_risk(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserSnapshotHandler::new(
                browser_backend.clone(),
            )),
            "📸",
            vec![],
            RiskLevel::Safe,
        );
        reg(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserClickHandler::new(
                browser_backend.clone(),
            )),
            "🖱️",
            vec![],
        );
        reg(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserTypeHandler::new(
                browser_backend.clone(),
            )),
            "⌨️",
            vec![],
        );
        reg(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserScrollHandler::new(
                browser_backend.clone(),
            )),
            "📜",
            vec![],
        );
        reg(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserBackHandler::new(
                browser_backend.clone(),
            )),
            "⬅️",
            vec![],
        );
        reg(
                    &enabled_toolsets,
                    registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserPressHandler::new(
                browser_backend.clone(),
            )),
            "🔘",
            vec![],
        );
    }

    // -- Mixture of Agents ---------------------------------------------------

    // -- Process registry ----------------------------------------------------

    // -- Transcription -------------------------------------------------------

    // -- Voice mode ----------------------------------------------------------

    // -- TTS Premium ---------------------------------------------------------

    // -- OSV Check -----------------------------------------------------------

    // -- URL Safety ----------------------------------------------------------
    reg_risk(
        &enabled_toolsets,
        registry,
        "security",
        Arc::new(crate::tools::url_safety::UrlSafetyHandler::default()),
        "🔒",
        vec![],
        RiskLevel::Safe,
    );

    // -- Env passthrough -----------------------------------------------------

    // -- Credential files ----------------------------------------------------

    // -- Tool result storage -------------------------------------------------

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());

    // Post-pass: if the caller passed an explicit
    // `enabled_toolsets` whitelist, flip every registered
    // toolset whose tag is *not* in the whitelist to
    // `is_enabled=false`. Done as a single walk over the
    // registered tags (not per-`reg` call) so we issue
    // O(toolsets) `set_toolset_enabled` calls instead of
    // O(tools). The previous per-`reg` filter (above) is kept
    // as a debug log so operators can still see which tools
    // were filtered in the log.
    if let Some(set) = &enabled_toolsets {
        // Mirror the "all"/"*" wildcard semantics from
        // `is_enabled` so the post-pass does not
        // accidentally disable everything when the wildcard
        // is present.
        let wildcard = set.contains("all") || set.contains("*");
        if !wildcard {
            let registered = registry.list_toolsets();
            for ts in registered {
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

    // Essential toolset floor: re-assert the toolsets the
    // LLM must always see (`terminal`, `file`,
    // `code_execution`, `web`, `memory`, `pdf`, `core`,
    // `security`) AFTER the whitelist post-pass above. If
    // the user's persisted `enabled_toolsets.json` omitted
    // `terminal` — e.g. a hand-edited list containing only
    // `["file", "web"]` — the post-pass would have just
    // turned `terminal` off, and the LLM would answer "I
    // don't have terminal". The floor undoes that, logging
    // each re-enable so operators can see when a saved
    // preference is being overridden.
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
        AgentError, CommandOutput, Skill, SkillMeta, SkillProvider, TerminalBackend,
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
            "homeassistant__list",
            "homeassistant__get",
            "homeassistant__list_services",
            "homeassistant__call_service",
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

/// Read `browser.allowed_domains` from `config.yaml`. Returns `None` when
/// the key is missing (no restriction). The schema is the same as the
/// `AgentLoopBehaviorConfig` style: a YAML list of host strings, with
/// `*.example.com` wildcards allowed.
fn browser_allowed_domains_from_config() -> Option<Vec<String>> {
    let home = std::env::var("HERMIX_HOME")
        .or_else(|_| std::env::var("HERMES_HOME"))
        .ok()?;
    let path = std::path::Path::new(&home).join("config.yaml");
    let text = std::fs::read_to_string(&path).ok()?;
    // Cheap hand-rolled YAML scan — we don't want a full YAML dep here
    // for one list. The expected shape is:
    //   browser:
    //     allowed_domains:
    //       - example.com
    //       - "*.trusted.io"
    let mut in_browser = false;
    let mut in_domains = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let trimmed_l = trimmed.trim_start();
        if !in_browser {
            // Match a top-level `browser:` key (no leading whitespace).
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed == "browser:" {
                in_browser = true;
            }
            continue;
        }
        // We are inside the `browser:` block. Detect a new top-level key
        // (i.e. a sibling that ends the block).
        if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
            in_browser = false;
            in_domains = false;
            continue;
        }
        if !in_domains {
            // Look for the `allowed_domains:` sub-key. We compare against
            // a left-trimmed copy so the leading indent of nested YAML
            // is harmless.
            if trimmed_l.starts_with("allowed_domains:") {
                // Inline list: `allowed_domains: [a, b, c]`
                if let Some(rest) = trimmed_l.strip_prefix("allowed_domains:") {
                    let rest = rest.trim();
                    if let Some(inner) = rest
                        .strip_prefix('[')
                        .and_then(|s| s.strip_suffix(']'))
                    {
                        for item in inner.split(',') {
                            let s = item.trim().trim_matches('"').trim_matches('\'');
                            if !s.is_empty() {
                                out.push(s.to_string());
                            }
                        }
                    }
                    in_domains = true;
                }
                continue;
            }
        } else {
            // Inside `allowed_domains:`: list items start with `- ` (after indent).
            if let Some(rest) = trimmed_l.strip_prefix("- ") {
                let s = rest.trim().trim_matches('"').trim_matches('\'');
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            } else if !trimmed_l.is_empty() {
                // Out of the list (a sibling sub-key).
                in_domains = false;
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Read `browser.cdp_port` from `~/.hermix/config.yaml`. Hand-rolled
/// YAML scan — we don't want a full YAML dep here for one int. The
/// expected shape is:
/// ```yaml
/// browser:
///   cdp_port: 9223
/// ```
/// `None` = fall back to `cdp_bootstrap::DEFAULT_CDP_PORT` (9222) or
/// `HERMIX_BROWSER_CDP_PORT` env var.
fn browser_cdp_port_from_config() -> Option<u16> {
    let home = std::env::var("HERMIX_HOME")
        .or_else(|_| std::env::var("HERMES_HOME"))
        .ok()?;
    let path = std::path::Path::new(&home).join("config.yaml");
    let text = std::fs::read_to_string(&path).ok()?;
    let mut in_browser = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or(""); // drop comments
        let trimmed_l = line.trim_start();
        // Detect entry into `browser:` block (top-level key).
        if !in_browser {
            if trimmed_l.starts_with("browser:") || trimmed_l.starts_with("browser :") {
                in_browser = true;
            }
            continue;
        }
        // Inside `browser:` — only the first-level sub-keys matter.
        // Indent is 0 or 2 spaces; trimmed_l has no leading whitespace.
        if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed_l.is_empty() {
            // Left the browser block.
            break;
        }
        if let Some(rest) = trimmed_l.strip_prefix("cdp_port:") {
            let rest = rest.trim();
            if let Ok(n) = rest.parse::<u16>() {
                return Some(n);
            }
        }
    }
    None
}

/// Read `browser.headless` from `~/.hermix/config.yaml`. `Some(bool)`
/// when set, `None` when missing. Env var `HERMIX_BROWSER_HEADLESS`
/// is read by `BrowserRegistry::headless()` as a secondary fallback
/// (so this config path wins over the env var).
fn browser_headless_from_config() -> Option<bool> {
    let home = std::env::var("HERMIX_HOME")
        .or_else(|_| std::env::var("HERMES_HOME"))
        .ok()?;
    let path = std::path::Path::new(&home).join("config.yaml");
    let text = std::fs::read_to_string(&path).ok()?;
    let mut in_browser = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed_l = line.trim_start();
        if !in_browser {
            if trimmed_l.starts_with("browser:") || trimmed_l.starts_with("browser :") {
                in_browser = true;
            }
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed_l.is_empty() {
            break;
        }
        if let Some(rest) = trimmed_l.strip_prefix("headless:") {
            let rest = rest.trim().to_ascii_lowercase();
            return match rest.as_str() {
                "true" | "yes" | "1" | "on" => Some(true),
                "false" | "no" | "0" | "off" => Some(false),
                _ => None,
            };
        }
    }
    None
}

#[cfg(test)]
mod phase3_registry_tests {
    use super::*;
    use std::io::Write;

    static TEST_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Each test gets its own temp dir. We can't share one because
    /// rust runs tests in parallel by default and they all read
    /// `HERMIX_HOME` to find the config file.
    fn write_config(content: &str) -> std::path::PathBuf {
        let n = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hermix-test-{}-{}",
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    /// `HERMIX_HOME` is process-global env state; concurrent tests
    /// reading/writing it race. Acquire this lock for the entire
    /// duration of any test that touches `HERMIX_HOME`.
    static HERMIX_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run a closure with `HERMIX_HOME` set to a unique temp dir,
    /// and restore the env on exit. Serialised across the test
    /// module via [`HERMIX_HOME_LOCK`] so we don't race with
    /// parallel cargo test threads.
    fn with_temp_config<F: FnOnce()>(content: &str, body: F) {
        let _guard = HERMIX_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = write_config(content);
        std::env::set_var("HERMIX_HOME", path.parent().unwrap());
        body();
        std::env::remove_var("HERMIX_HOME");
    }

    #[test]
    fn config_parses_allowed_domains_list() {
        with_temp_config(
            "browser:\n  allowed_domains:\n    - example.com\n    - \"*.trusted.io\"\n",
            || {
                let domains = browser_allowed_domains_from_config();
                assert_eq!(
                    domains,
                    Some(vec!["example.com".to_string(), "*.trusted.io".to_string()])
                );
            },
        );
    }

    #[test]
    fn config_returns_none_when_key_missing() {
        with_temp_config("other_key: 1\n", || {
            let domains = browser_allowed_domains_from_config();
            assert_eq!(domains, None);
        });
    }

    #[test]
    fn config_handles_inline_array_syntax() {
        with_temp_config("browser:\n  allowed_domains: [a.com, b.com]\n", || {
            let domains = browser_allowed_domains_from_config();
            assert_eq!(
                domains,
                Some(vec!["a.com".to_string(), "b.com".to_string()])
            );
        });
    }

    #[test]
    fn config_parses_cdp_port() {
        with_temp_config("browser:\n  cdp_port: 9223\n", || {
            let port = browser_cdp_port_from_config();
            assert_eq!(port, Some(9223));
        });
    }

    #[test]
    fn config_returns_none_for_missing_cdp_port() {
        with_temp_config("browser:\n  allowed_domains: [a.com]\n", || {
            let port = browser_cdp_port_from_config();
            assert_eq!(port, None);
        });
    }

    #[test]
    fn config_parses_headless_true() {
        with_temp_config("browser:\n  headless: true\n", || {
            let h = browser_headless_from_config();
            assert_eq!(h, Some(true));
        });
    }

    #[test]
    fn config_parses_headless_false() {
        with_temp_config("browser:\n  headless: false\n", || {
            let h = browser_headless_from_config();
            assert_eq!(h, Some(false));
        });
    }
}
