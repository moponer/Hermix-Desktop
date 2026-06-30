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
pub mod browser_supervisor;
pub mod cdp_bootstrap;
pub mod cdp_supervisor;
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

use crate::ToolRegistry;

/// Register all migrated tools via their self-registration functions.
/// During migration, unmigrated tools still go through `register_builtins.rs`.
/// Once all tools are migrated, `register_builtins.rs` is deleted.
pub fn register_all(registry: &ToolRegistry) {
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
    // TODO: migrate more tools here as they get their `register()` fn
}
