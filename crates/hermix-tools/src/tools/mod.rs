//! Tool implementations module
//!
//! Each submodule implements one or more tool handlers that conform
//! to the `ToolHandler` trait from `hermes-core`.

pub mod audio_gen;
pub mod aliases;
pub mod browser;
pub mod browser_supervisor;
pub mod cdp_bootstrap;
pub mod cdp_supervisor;
pub mod clarify;
pub mod computer_use;
pub mod code_execution;
pub mod credential_files;
pub mod cronjob;
pub mod cua;
pub mod delegation;
pub mod disk_cleanup;
pub mod env_passthrough;
pub mod file;
pub mod gpt_image;
pub mod hermix_achievements;
pub mod homeassistant;
pub mod image_gen;
pub mod kanban;
pub mod managed_tool_gateway;
pub mod media_workflow;
pub mod memory;
pub mod messaging;
pub mod mixture_of_agents;
pub mod model_providers;
pub mod osv_check;
pub mod office_cli;
pub mod pdf;
pub mod platforms;
pub mod process_registry;
pub mod security_guidance;
pub mod session_search;
pub mod skill_commands;
pub mod skill_utils;
pub mod skills;
pub mod terminal;
pub mod todo;
pub mod tool_result_storage;
pub mod tool_search;
pub mod tool_chain;
pub mod transcription;
pub mod tts;
pub mod tts_premium;
pub mod url_safety;
pub mod video_gen;
pub mod vision;
pub mod voice_mode;
pub mod web;
