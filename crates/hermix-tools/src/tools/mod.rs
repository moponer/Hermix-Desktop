//! Tool implementations module
//!
//! Each submodule implements one or more tool handlers that conform
//! to the `ToolHandler` trait from `hermes-core`.
//!
//! Cleanup history (2026-06-30, Plan v2):
//! - Removed dead tools: audio_gen, tts_premium, transcription, voice_mode,
//!   process_registry, credential_files, env_passthrough, osv_check,
//!   managed_tool_gateway, mixture_of_agents, tool_result_storage.
//!   These were marked ❌ in TOOL_CLEANUP.md but their files were never
//!   physically deleted (only de-registered). P1.1 finally removes them.
//! - Removed `pub mod process_registry / env_passthrough / credential_files
//!   / osv_check / managed_tool_gateway / mixture_of_agents
//!   / tool_result_storage` references that pointed at the deleted files.

pub mod aliases;
pub mod browser;
pub mod cdp_bootstrap;
pub mod clarify;
pub mod code_execution;
pub mod cronjob;
pub mod cua;
pub mod delegation;
pub mod disk_cleanup;
pub mod file;
pub mod gpt_image;
pub mod hermix_achievements;
pub mod homeassistant;
pub mod image_gen;
// pub mod jira; // Untracked, broken - removed until fixed
pub mod kanban;
pub mod media_workflow;
pub mod memory;
pub mod messaging;
pub mod model_providers;
pub mod office_cli;
pub mod pdf;
pub mod platforms;
pub mod security_guidance;
pub mod session_search;
pub mod skill_commands;
pub mod skill_utils;
pub mod skills;
// pub mod slack; // Untracked, broken - removed until fixed
pub mod terminal;
pub mod todo;
pub mod tool_search;
pub mod tool_chain;
pub mod tts;
pub mod url_safety;
pub mod video_gen;
pub mod vision;
pub mod voice_mode;
pub mod web;

use std::sync::Arc;

use hermix_core::{SkillProvider, TerminalBackend};

use crate::tools::browser::BrowserBackend;
use crate::ToolRegistry;

/// Register all migrated tools via their self-registration functions.
/// During migration, unmigrated tools still go through `register_builtins.rs`.
/// Once all tools are migrated, `register_builtins.rs` is deleted.
///
/// P1.3 (2026-07-02) signature change: `register_all` now accepts the
/// three backends that tools needed but didn't fit into a default
/// constructor:
/// - `terminal_backend`: shared by `terminal`, `read_file`,
///   `write_file` (and reachable from `delegate_task` / shell exec).
///   `register_builtin_tools` is the single owner of this handle; if
///   you call `register_all` from elsewhere, pass a backend of the
///   same kind you'd pass to `register_builtin_tools`.
/// - `skill_provider`: backs the three `skills_*` tools.
/// - `browser_backend`: backs the ten `browser_*` tools. The
///   `register_builtin_tools` caller constructs a
///   `SessionedBrowserBackend` wrapping a `BrowserRegistry` so
///   group-chat sessions get isolated CDP state; tests can pass any
///   `Arc<dyn BrowserBackend>`.
pub fn register_all(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    browser_backend: Arc<dyn BrowserBackend>,
) {
    // P1.2 per-tool migration (completed 2026-07-01):
    disk_cleanup::register(registry);
    vision::register(registry);
    security_guidance::register(registry);
    platforms::register(registry);
    cronjob::register(registry);
    todo::register(registry);
    tts::register(registry);
    messaging::register(registry);
    image_gen::register(registry);
    video_gen::register(registry);
    homeassistant::register(registry);
    session_search::register(registry);
    hermix_achievements::register(registry);
    kanban::register(registry);
    model_providers::register(registry);
    url_safety::register(registry);
    clarify::register(
        registry,
        Arc::new(crate::backends::clarify::SignalClarifyBackend::new())
            as Arc<dyn clarify::ClarifyBackend>,
    );
    delegation::register(
        registry,
        Arc::new(crate::backends::delegation::SignalDelegationBackend::new())
            as Arc<dyn delegation::DelegationBackend>,
    );
    media_workflow::register(
        registry,
        Arc::new(media_workflow::PlanningWorkflowBackend::new())
            as Arc<dyn media_workflow::MediaWorkflowBackend>,
    );
    tool_chain::register(registry, Arc::new(registry.clone()));
    code_execution::register(
        registry,
        Arc::new(crate::backends::code_execution::LocalCodeExecutionBackend::default())
            as Arc<dyn code_execution::CodeExecutionBackend>,
    );
    cua::register(registry);
    // P1.3 (2026-07-02): file tools (read_file / write_file / patch /
    // search_files) share `terminal_backend` with the terminal tool so
    // every code path that opens files uses the same backend. Patch +
    // search get local backends (LocalPatchBackend / LocalSearchBackend)
    // because they need fuzzy-match + ripgrep semantics that the
    // TerminalBackend trait doesn't expose.
    file::register(registry, terminal_backend.clone());
    // P1.8 (2026-07-02): office_cli was previously implemented but
    // never registered. Add it next to `file` and `terminal` so the
    // `essential::ESSENTIAL_TOOLSETS["office"]` floor finds a real
    // tool to re-assert and `set_enabled_toolsets("office", false)`
    // actually disables something.
    office_cli::register(registry, terminal_backend.clone(), registry.terminal_workspace_handle());
    // P1.8 (2026-07-02): `read_pdf` was previously tagged
    // `"file"` and *never wired into `register_all`*, so the LLM
    // had no PDF-reading tool at all. Re-tag it with the dedicated
    // `"pdf"` toolset (matches `essential::ESSENTIAL_TOOLSETS`) and
    // add the call here.
    pdf::register(registry);
    // P1.3: terminal tool. The workspace handle is owned by the
    // registry (set by `LocalAgentService` per-turn from
    // `AgentOverrides::workspace`) so the terminal tool sees the
    // same cwd the desktop UI picker picked.
    terminal::register(registry, terminal_backend, registry.terminal_workspace_handle());
    // P1.3: web tools (keyless local backends: Baidu HTML scraper +
    // reqwest HTML fetch).
    web::register(registry);
    // P1.3: list_files / edit_file (aliases of search_files / patch)
    // + list_tools (introspection).
    aliases::register(registry);
    // P1.3: memory tool with FileMemoryBackend.
    memory::register(registry);
    // P1.3: skills tools (skills_list / skill_view / skill_manage).
    skills::register(registry, skill_provider);
    // slack::register(registry); // Untracked, broken - removed until fixed
    // P1.3: browser tools (10 handlers backed by the sessioned
    // backend so per-session CDP state stays isolated).
    browser::register(registry, browser_backend);
    // jira::register(registry); // Untracked, broken - removed until fixed
}
