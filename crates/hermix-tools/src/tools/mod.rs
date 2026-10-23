//! Tool implementations module
//!
//! Each submodule implements one or more tool handlers that conform
//! to the `ToolHandler` trait from `hermes-core`.
//!
//! Cleanup history (, Plan v2):
//! - Removed dead tools: audio_gen, tts_premium, transcription, voice_mode,
//!   process_registry, credential_files, env_passthrough, osv_check,
//!   managed_tool_gateway, mixture_of_agents, tool_result_storage.
//!   These were marked ❌ in TOOL_CLEANUP.md but their files were never
//!   physically deleted (only de-registered). P1.1 finally removes them.
//! - Removed `pub mod process_registry / env_passthrough / credential_files
//!   / osv_check / managed_tool_gateway / mixture_of_agents
//!   / tool_result_storage` references that pointed at the deleted files.

pub mod aliases;
pub mod clarify;
pub mod code_execution;
pub mod cronjob;
pub mod custom_app_manager;
pub mod daily_memory;
pub mod delegation;
pub mod get_subagent_result;
pub mod disk_cleanup;
pub mod file;
pub mod file_read_state;
pub mod plan_mode;
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
pub mod markitdown;
pub mod pdf;
pub mod platforms;
pub mod security_guidance;
pub mod session_search;
pub mod session_stamper;
pub mod skill_commands;
pub mod skill_utils;
pub mod skills;
pub mod storage_analyzer;
pub mod storage_clean;
// pub mod slack; // Untracked, broken - removed until fixed
pub mod terminal;
pub mod todo;

pub mod task_tracker;
pub mod approval_tokens;
pub mod dsh_engine_fetch;
pub mod file_converter;
pub mod html_to_docx;
pub mod office_export;
pub mod tool_search;
pub mod tool_chain;
pub mod tts;
pub mod url_safety;
pub mod vendor;
pub mod bbs;
pub mod vault;
pub mod workbench_manager;
pub mod video_gen;
pub mod voice_mode;
pub mod web;
pub mod os_auto;
pub mod workflow;

    use std::sync::Arc;

    use hermix_core::{SkillProvider, TerminalBackend};

    use crate::ToolRegistry;
    // `clarify::ClarifyBackend` is no longer imported here — clarify
    // registration moved entirely to `register_builtin_tools`.

/// Register all migrated tools via their self-registration functions.
/// During migration, unmigrated tools still go through `register_builtins.rs`.
/// Once all tools are migrated, `register_builtins.rs` is deleted.
/// P1.3 signature change: `register_all` now accepts the
/// two backends that tools needed but didn't fit into a default
/// constructor:
/// - `terminal_backend`: shared by `terminal`, `read_file`,
/// `write_file` (and reachable from `delegate_task` / shell exec).
/// `register_builtin_tools` is the single owner of this handle; if
/// you call `register_all` from elsewhere, pass a backend of the
/// same kind you'd pass to `register_builtin_tools`.
/// - `skill_provider`: backs the three `skills_*` tools.
pub fn register_all(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    todo_event_tx: Option<tokio::sync::broadcast::Sender<todo::TodoChange>>,
    // 接缝：主进程装配（hermix-context::boot）注入 fs/shell。`None` 时
    // file/terminal 工具退回老路径。tests 不需要传。
    seams: Option<hermix_context::BootSeams>,
) {
    //P1.2 per-tool migration (completed ):
    disk_cleanup::register(registry);
    storage_analyzer::register(registry);
    storage_clean::register(registry);
    security_guidance::register(registry);
    platforms::register(registry);
    cronjob::register(registry);
    workflow::register(registry);
    todo::register(registry, todo_event_tx);
    // Plan mode（ZCode Enter/ExitPlanMode analog）：只读调研阶段，
    // 写/执行类工具在 do_tool_turn 派发前被拒绝。
    plan_mode::register(registry);
    // task_manager 已移除（2026-10-18 能力对齐审计）：内存态 Vec 无持久化
    // （重启即失的残品），与 todo/task_tracker/kanban 三套任务机制重叠。
    // 依据见 docs/notes/proposed/architecture/2026-10-18-capability-alignment-audit.md。
    approval_tokens::register(registry);
    file_converter::register(registry);
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
    // NOTE: `clarify` is deliberately NOT registered here. It is
    // registered by `register_builtin_tools` after `register_all`
    // returns, because the caller may supply a custom backend (e.g.
    // the desktop's `ChannelClarifyBackend`). Registering here with
    // the signal default would cause a duplicate-registration error
    // when `register_builtin_tools` tries to override it.
    delegation::register(
        registry,
        Arc::new(crate::backends::delegation::SignalDelegationBackend::new())
            as Arc<dyn delegation::DelegationBackend>,
    );
    get_subagent_result::register(registry);
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
    vendor::register(registry);    //P1.3 : file tools (read_file / write_file / patch /
    // search_files) share `terminal_backend` with the terminal tool so
    // every code path that opens files uses the same backend. Patch +
    // search get local backends (LocalPatchBackend / LocalSearchBackend)
    // because they need fuzzy-match + ripgrep semantics that the
    // TerminalBackend trait doesn't expose.
    file::register(registry, terminal_backend.clone(), seams.as_ref().map(|s| s.fs.clone()));
    // office_export: 二进制 Office 导出（2026-10-17 决策笔记批次 1，XLSX 先行）。
    // 纯 Rust OOXML 写入——docx/pptx 批次未落地前 format 参数显式拒绝。
    office_export::register(registry);
    // html_to_docx: HTML→Word 转换（2026-10-17 稳定性与组装优化方案 P3），
    // 补齐"模型输出 HTML 但要交付 .docx"的转换缺口，纯 Rust 零新依赖。
    html_to_docx::register(registry);
    //P1.8-style : `markitdown` converts arbitrary local documents
    // (PDF/PPTX/DOCX/XLSX/HTML/EPUB/CSV/JSON/XML/MSG/ZIP/image) into
    // compact LLM-friendly Markdown — the token-saving file reader.
    // Tagged with the dedicated `"document"` toolset (matches
    // `essential::ESSENTIAL_TOOLSETS`) so the floor can re-assert it and
    // a user `enabled_toolsets.json` cannot silently disable it.
    markitdown::register(registry, terminal_backend.clone());
    //P1.8 : `read_pdf` was previously tagged
    // `"file"` and *never wired into `register_all`*, so the LLM
    // had no PDF-reading tool at all. Re-tag it with the dedicated
    // `"pdf"` toolset (matches `essential::ESSENTIAL_TOOLSETS`) and
    // add the call here.
    pdf::register(registry);
    // P1.3: terminal tool. The workspace handle is owned by the
    // registry (set by `LocalAgentService` per-turn from
    // `AgentOverrides::workspace`) so the terminal tool sees the
    // same cwd the desktop UI picker picked. The approval-ctx
    // handle is also owned by the registry; `LocalAgentService`
    // writes a fresh `TerminalApprovalContext` per turn so the
    // terminal's `RequiresConfirmation` path can emit
    //`request_approval` events on the active WS channel .
    terminal::register(
        registry,
        terminal_backend,
        registry.terminal_workspace_handle(),
        registry.terminal_approval_ctx_handle(),
        seams.as_ref().map(|s| s.shell.clone()),
        //TODO(-terminal): pass registry.terminal_permission_profile_handle
        // once `TerminalHandler::new` accepts the handle and gates commands/paths
        // against `PermissionProfile.granted_commands` / `granted_paths`. Until
        // then the terminal tool always prompts regardless of profile state.
    );
    // P1.3: web tools (keyless local backends: Baidu HTML scraper +
    // reqwest HTML fetch).
    web::register(registry);
    // Native OS automation (2026-10-13 note): browser line over CDP +
    // desktop line over UI Automation COM. `desktop` stays OFF until
    // the operator enables it in Settings (os_auto::OPT_IN_TOOLSETS);
    // `browser` is on the essential hard floor — see
    // essential::HARD_ESSENTIAL_TOOLSETS.
    os_auto::register(registry);
    // P1.3: list_files / edit_file (aliases of search_files / patch)
    // + list_tools (introspection).
    aliases::register(registry);
    // P1.3: memory tool with FileMemoryBackend.
    memory::register(registry);
    daily_memory::register(registry);
    // P1.3: skills tools (skills_list / skill_view / skill_manage).
    skills::register(registry, skill_provider);
    // slack::register(registry); // Untracked, broken - removed until fixed
    // Deferred-tool bridge (tool_search / tool_describe / tool_call).
    // Was never called from here, so the handlers existed but stayed
    // unreachable — see `tools::tool_search::register` for the full
    // three-layer chain that made this invisible to the model.
    tool_search::register(registry);
    // jira::register(registry); // Untracked, broken - removed until fixed
}
