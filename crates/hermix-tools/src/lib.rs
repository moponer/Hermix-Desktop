#![allow(
    clippy::field_reassign_with_default,
    clippy::manual_clamp,
    clippy::manual_strip,
    clippy::too_many_arguments,
    clippy::type_complexity,
    dead_code,
    noop_method_call
)]
//! # hermes-tools
//!
//! Tool registry, toolset system, and tool implementations for Hermes Agent.
//!
//! This crate provides:
//! - **ToolRegistry**: Central registry for all available tools with availability checks
//! - **ToolsetManager**: Manages named groups of tools (toolsets) with recursive resolution
//! - **Tool dispatch**: Parallel execution of multiple tool calls with budget enforcement
//! - **Tool implementations**: Concrete handlers for web, terminal, file, and more
//! - **Approval system**: Dangerous command pattern detection for terminal safety

pub mod approval;
pub mod backup;
pub mod backends;
pub mod cached_check;
pub mod context;
pub mod dispatch;
pub mod essential;
pub mod register_builtins;
pub mod registry;
// `sandbox` module removed — 491 lines of dead code with zero
// external callers and zero integration into `ApprovalEngine`. The full
// sandbox/approval wiring will be rebuilt the next time a tool actually
// needs it; until then, removing the half-fabricated scaffold prevents
// future drift between declared interface and runtime behavior.

pub mod param_coercion;
pub mod tool_meta;
pub mod tools;
pub mod toolset;
pub mod toolset_distributions;
pub mod tts_streaming;
pub mod v4a_patch;

// Re-export registry types
pub use registry::{EffectSink, ToolEntry, ToolEntryInfo, ToolRegistry};

// Re-export cached_check helper
pub use cached_check::{cached_check, cached_check_default, CheckFn, DEFAULT_TTL};

// Re-export toolset types
pub use toolset::{Toolset, ToolsetError, ToolsetManager};

// Re-export dispatch
pub use dispatch::{dispatch_single, dispatch_tools, DispatchedResult};

// Re-export approval types
pub use approval::{
    check_approval, extract_trust_signature, is_readonly_command, ApprovalDecision,
    ApprovalManager, TerminalApprovalContext, UserAllowlist, UserAllowlistEntry,
};

//Re-export context types
pub use context::{ContextAware, ToolCallContext};

// Re-export credential guard
pub mod credential_guard;
pub mod deferred_loader;
pub use credential_guard::CredentialGuard;

// Re-export all tool handler implementations and their backend traits
pub use tools::clarify::{ClarifyBackend, ClarifyHandler};
pub use tools::code_execution::{CodeExecutionBackend, ExecuteCodeHandler};
pub use tools::cronjob::{CronjobBackend, CronjobHandler};
pub use tools::workflow::{
    get_workflow, list_workflows, remove_workflow, update_run_status, upsert_workflow,
    WorkflowEntry,
};
pub use tools::delegation::{DelegateTaskHandler, DelegationBackend};
pub use tools::file::{
    PatchBackend, PatchHandler, ReadFileHandler, SearchBackend, SearchFilesHandler,
    WriteFileHandler, read_image_for_llm,
};
pub use tools::homeassistant::{
    HaCallServiceHandler, HaGetStateHandler, HaListEntitiesHandler, HaListServicesHandler,
    HomeAssistantBackend,
};
pub use tools::image_gen::{ImageGenBackend, ImageGenerateHandler};
pub use tools::memory::{MemoryBackend, MemoryHandler};
// Native OS automation (2026-10-13 note). Re-exported so
// `hermix-server`'s toolset reconciliation can apply the same
// "opt-in toolsets start off" rule at startup that
// `register_builtin_tools` applies at registration time — the two
// paths must agree or a restart silently turns them back on.
pub use tools::os_auto::{
    apply_opt_in_defaults, BROWSER_TOOLSET, DESKTOP_TOOLSET, OPT_IN_TOOLSETS,
};
pub use tools::messaging::{MessagingBackend, SendMessageHandler};
pub use tools::session_search::{SessionSearchBackend, SessionSearchHandler};
pub use tools::session_stamper::SessionStamper;
pub use tools::skill_commands;
pub use tools::skill_utils;
pub use tools::skills::{SkillManageHandler, SkillViewHandler, SkillsListHandler};
pub use tools::terminal::TerminalHandler;
pub use tools::todo::{TodoBackend, TodoHandler};
pub use tools::tool_search::{
    assemble_tool_schemas, classify_tools, classify_tools_with_meta, is_bridge, AssemblyResult,
    ToolCallHandler, ToolDescribeHandler, ToolSearchHandler,
};
pub use tools::tts::{TextToSpeechHandler, TtsBackend};
pub use tools::url_safety::{TirithAction, TirithCheckResult, TirithConfig, TirithRule, TirithSecurity, UrlSafetyHandler};
pub use tools::web::{WebExtractBackend, WebExtractHandler, WebSearchBackend, WebSearchHandler};

// Re-export real backend implementations
pub use backends::clarify::{
    ChannelClarifyBackend, ClarifyDispatcher, ClarifySessionProvider, PendingClarify,
    PendingClarifySnapshot, SignalClarifyBackend,
};
pub use backends::code_execution::LocalCodeExecutionBackend;
pub use backends::cronjob::SignalCronjobBackend;
pub use backends::delegation::{RpcDelegationBackend, SignalDelegationBackend};
pub use backends::file::{LocalPatchBackend, LocalSearchBackend};
pub use backends::homeassistant::HaRestBackend;
pub use backends::image_gen::FalImageGenBackend;
pub use backends::memory::FileMemoryBackend;
pub use backends::messaging::SignalMessagingBackend;
pub use backends::session_search::SqliteSessionSearchBackend;
pub use backends::todo::FileTodoBackend;
pub use backends::tts::MultiTtsBackend;
pub use backends::web::{BaiduHtmlSearchBackend, SimpleExtractBackend};

//Re-export tool metadata types
pub use tool_meta::{LoadingStrategy, ToolMeta, ToolMetaRegistry, DEFAULT_RESIDENT_TOOLS};

//Re-export deferred loader types
pub use deferred_loader::{assemble_tool_schemas_with_meta, DeferredToolLoader};

// Re-export builtin registration helper
pub use register_builtins::register_builtin_tools;

// Re-export core types needed by consumers
pub use hermix_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};
