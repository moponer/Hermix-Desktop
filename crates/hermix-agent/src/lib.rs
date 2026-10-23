#![allow(
    clippy::collapsible_match,
    clippy::collapsible_if,
    clippy::doc_nested_refdefs,
    clippy::if_same_then_else,
    clippy::manual_clamp,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod acui_dispatch;
pub mod agent_builder;
pub mod agent_config;
pub mod agent_helpers;
pub mod agent_loop;
pub mod archive;
pub mod api_bridge;
pub mod approval_diff;
pub mod auxiliary_builder;
pub mod budget;
pub mod consciousness;
pub mod compression;
pub mod compression_ops;
pub mod compression_pipeline;
pub mod json_crusher;
pub mod semantic_dedup;
pub use hermix_tools::spill;
pub mod context;
pub mod context_files;
pub mod context_pressure;
pub mod directions;
pub mod context_references;
pub mod copilot_acp;
pub mod cost_guard;
pub mod cost_routing;
pub mod credential_pool;
pub mod environment;
pub mod fallback;
pub mod focus_stack_ops;
pub mod gatherer;
pub mod hardline;
pub mod hook_dispatch;
pub mod interrupt;
pub mod local_agent_service;
pub mod llm_retry;
pub mod memory_layer_adapter;
pub mod memory_hooks;
pub mod memory_prefetch;
pub mod message_cache;
pub mod message_helpers;
pub mod native_plugin;
pub mod no_op_compression_engine;
pub mod oauth_refresh;
pub mod orchestrator;
pub mod pii_redact;
pub mod plugins;
pub mod empty_response_handler;
pub mod prompt_assembly;
pub mod provider;
pub mod python_alignment;
pub mod rate_limit;
pub mod reasoning;
pub mod run_loop;
pub mod runtime_provider;
pub mod retry_counters;
pub mod session_persistence;
pub mod session_lifecycle;
pub mod skill_orchestrator;
pub mod skill_loader;
/// Declarative sub-agent profiles (`<home>/agents/*.md`).
pub mod agent_profiles;
pub mod skill_health;
pub mod skill_auto_create;
pub mod skill_patch;
pub mod feedback_loop;
pub mod self_improvement;
pub mod researcher_os;
pub mod benchmark_suite;
pub mod cognition_arch;
pub mod context_engine;
pub mod smart_model_routing;
pub mod stream_collect;
pub mod steer;
pub mod sub_agent_orchestrator;
pub mod tool_result_processor;
pub mod tool_call_validator;
pub mod convergence_detector;
pub mod turn_report;
pub mod terminal_filter;
pub mod tool_dispatch;
pub mod turn_timings;
pub mod sub_agent_safety;
pub mod subdirectory_hints;
pub mod tool_executor;
pub mod reflection_generator;
pub mod recall_scheduler;
pub mod flush_memories;
pub mod tool_error_demotion;
pub mod harness_self_edit;
/// Harness² 的任务级递归：Judge → Probe → Compose（多次执行对比提炼技能）。
pub mod harness_probe;
pub mod acceptance_gate;
pub mod experience_archive;

//New capabilities
pub mod bdi;
pub mod latent_briefing;
pub mod long_horizon;

//Researcher OS
pub mod researcher_loop;
pub mod researcher_queue;

// Re-export primary agent types
pub use agent_loop::{
    AgentCallbacks, AgentConfig, AgentLoop, ApiMode, CheapModelRouteConfig, ErrorClass,
    RetryConfig, SmartModelRoutingConfig, TurnMetrics,
};

// Re-export context management
pub use compression::summarize_messages_with_llm;
// Re-export circuit-breaker types (Day 4)
pub use compression::{
    BreakerSnapshot, BreakerState, DEFAULT_BREAKER_COOLDOWN_SECS, DEFAULT_BREAKER_THRESHOLD,
};
pub use context::{
    load_context_files, load_soul_md, load_soul_md_from, switch_personality, ContextManager,
    SystemPromptBuilder, SELF_EVOLUTION_GUIDANCE,
};

//Re-export v2 short-term context unload . `read_session_index`
// powers the server endpoint `GET /v1/sessions/{id}/archive-refs` so the
// front-end can list the refs without re-scanning every archive file.
pub use archive::{
    archive_context_section, read_session_index, render_archive_summary, ArchiveError, ArchiveRef,
};

// Re-export runtime environment (host-OS + hermix-home snapshot for
// the system prompt)
pub use environment::{build_environment_block, RuntimeEnvironment};

// Re-export budget enforcement
pub use budget::{
    check_aggregate_budget, enforce_budget, enforce_budget_with_spill, truncate_result,
};

// Re-export LLM providers
pub use api_bridge::CodexProvider;
pub use auxiliary_builder::{build_auxiliary_client_with_config, build_default_auxiliary_client, AuxiliaryWiringSummary};
pub use provider::{AnthropicProvider, GenericProvider, OpenRouterProvider};

// Re-export rate limiting, credential pool, and fallback chain
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth_refresh::{OAuthRefresh, OAuthStoreCredential};
pub use rate_limit::RateLimitTracker;

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;

//T1-B (completion ): removed `memory_manager.rs` entirely
// (the `MemoryProviderPlugin` trait was its sole remaining occupant).
// All write-path hooks now route through `MemoryRouter`, which fans
// out to every registered `MemoryLayer` backend — single trait, single
// fan-out path. All external memory plugins (Hindsight / Honcho / Mem0 /
// RetainDB / ByteRover / OpenViking / SuperMemory / Holographic) were
// culled — the project now relies solely on the built-in
// `EmbeddedMemoryBackend` (SQLite-backed `memory.db`) for persistent
// memory.

// Re-export plugin system
pub use plugins::{install_plugin_tools_into_registry, Plugin, PluginManager, PluginMeta};

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

//Re-export skill loader
pub use skill_loader::{SkillLoader, SkillMatch};

// Re-export session persistence
pub use session_persistence::{
    leading_system_prompt_for_persist, ApprovalRecord, SessionEntry, SessionPersistence,
};

// Re-export context files
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};

// Re-export subdirectory hints
pub use subdirectory_hints::{generate_project_hints, SubdirectoryHintTracker};

// Python `run_agent.py` alignment helpers (budget strip/inject, surrogate sanitize)
pub use python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    looks_like_codex_intermediate_ack, sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, CODEX_CONTINUE_USER_MESSAGE,
};

// Re-export sub-agent orchestrator
pub use sub_agent_orchestrator::{
    SubAgentLineage, SubAgentOrchestrator, SubAgentOrchestratorConfig, SubAgentRequest,
    SubAgentStatus, TaskRecordSink,
};

// Re-export local agent service
pub use local_agent_service::LocalAgentService;

//Re-export consciousness stream
pub use consciousness::{
    default_amplitude, ConsciousnessSample, ConsciousnessState, ConsciousnessTracker, VitalBucket,
    Vitals,
};

// Re-export core types that consumers need
pub use hermix_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk, ToolCall, ToolError,
    ToolResult, ToolSchema, UsageStats,
};
