#![allow(clippy::field_reassign_with_default, clippy::manual_strip)]
//! # hermix-config
//!
//! Configuration management for the hermes-agent system.
//!
//! Handles loading, merging, validating, and providing access to all
//! configuration sources including YAML, JSON, environment variables,
//! and sensible defaults.

pub mod config;
pub mod loader;
pub mod managed_gateway;
pub mod merge;
pub mod migration;
pub mod paths;
pub mod platform;
mod python_platform_env;
mod python_yaml_compat;
pub mod roundtrip_tests;
pub mod session;
pub mod streaming;

// Re-export key types for convenience
pub use config::{
    AdvancedSettingsConfig, AgentLoopBehaviorConfig, ApprovalConfig, CheapModelRouteConfig,
    CompactionConfig, GatewayConfig, LlmProviderConfig, McpServerEntry, ModelSelectorConfig,
    ModelSelectorEntry, ProfileConfig,
    ProxyConfig, SkillsSettings, SmartModelRoutingConfig, TerminalBackendType,
    TerminalConfig, ToolCapabilityConfig, ToolsSettings,
};
pub use loader::{
    apply_user_config_patch, auto_correct_model, default_model_for_provider,
    env_var_name_for_provider, load_config, load_user_config_file, provider_api_key_from_env,
    provider_has_key, save_config_yaml, user_config_field_display, validate_config,
    AutoCorrectReport, ConfigError,
};
pub use managed_gateway::{
    build_vendor_gateway_url, coerce_modal_mode, env_var_enabled, get_tool_gateway_scheme,
    has_direct_modal_credentials, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    prefers_gateway, read_nous_access_token, resolve_managed_tool_gateway,
    resolve_modal_backend_state, resolve_openai_audio_api_key, GatewayBuilder, GatewaySchemeError,
    ManagedToolGatewayConfig, ModalBackendState, ModalMode, NousProviderState, ResolveOptions,
    SelectedBackend, TokenReader, DEFAULT_TOOL_GATEWAY_DOMAIN,
};
pub use merge::{deep_merge, merge_configs};
pub use migration::{
    is_skipped, mark_skipped, clear_skipped, candidate_legacy_homes, user_home, LegacyArtifact,
    LegacyMigrator, LegacySnapshot, MigrationReport, SkippedRecord,
};
pub use paths::{
    cli_config_path, config_path, cron_dir, env_compat, env_path, gateway_json_path,
    gateway_pid_path, gateway_pid_path_in, hermix_home, memory_path, sessions_db_path,
    sessions_db_path_in, sessions_dir, sessions_dir_in, skills_dir, state_dir, trajectories_dir,
    trajectories_dir_in, user_path,
};
pub use platform::{extra_string, platform_token_or_extra, PlatformConfig, UnauthorizedDmBehavior};
pub use session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
pub use streaming::StreamingConfig;
