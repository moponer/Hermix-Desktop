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
//! - **Tool implementations**: Concrete handlers for web, terminal, file, browser, and more
//! - **Approval system**: Dangerous command pattern detection for terminal safety

pub mod approval;
pub mod backends;
pub mod cached_check;
pub mod dispatch;
pub mod essential;
pub mod register_builtins;
pub mod registry;
pub mod tools;
pub mod toolset;
pub mod toolset_distributions;
pub mod tts_streaming;
pub mod v4a_patch;

// Re-export registry types
pub use registry::{ToolEntry, ToolEntryInfo, ToolRegistry};

// Re-export cached_check helper
pub use cached_check::{cached_check, cached_check_default, CheckFn, DEFAULT_TTL};

// Re-export toolset types
pub use toolset::{Toolset, ToolsetError, ToolsetManager};

// Re-export dispatch
pub use dispatch::{dispatch_single, dispatch_tools, DispatchedResult};

// Re-export approval types
pub use approval::{
    check_approval, ApprovalDecision, ApprovalManager, UserAllowlist, UserAllowlistEntry,
};

// Re-export credential guard
pub mod credential_guard;
pub use credential_guard::CredentialGuard;

// Re-export all tool handler implementations and their backend traits
pub use tools::browser::{
    BrowserBackend, BrowserClickHandler, BrowserNavigateHandler, BrowserPressHandler,
    BrowserScrollHandler, BrowserSnapshotHandler, BrowserTypeHandler,
};
pub use tools::clarify::{ClarifyBackend, ClarifyHandler};
pub use tools::code_execution::{CodeExecutionBackend, ExecuteCodeHandler};
pub use tools::cronjob::{CronjobBackend, CronjobHandler};
pub use tools::cua::{
    CuaLeftClickHandler, CuaPressKeyHandler, CuaScreenInfoHandler,
    CuaScreenshotHandler, CuaTypeTextHandler, CuaUiClickHandler, CuaUiTreeHandler,
    CuaOcrRecognizeHandler,
};
pub use tools::delegation::{DelegateTaskHandler, DelegationBackend};
pub use tools::file::{
    PatchBackend, PatchHandler, ReadFileHandler, SearchBackend, SearchFilesHandler,
    WriteFileHandler,
};
pub use tools::homeassistant::{
    HaCallServiceHandler, HaGetStateHandler, HaListEntitiesHandler, HaListServicesHandler,
    HomeAssistantBackend,
};
pub use tools::image_gen::{ImageGenBackend, ImageGenerateHandler};
pub use tools::memory::{MemoryBackend, MemoryHandler};
pub use tools::messaging::{MessagingBackend, SendMessageHandler};
pub use tools::session_search::{SessionSearchBackend, SessionSearchHandler};
pub use tools::skill_commands;
pub use tools::skill_utils;
pub use tools::skills::{SkillManageHandler, SkillViewHandler, SkillsListHandler};
pub use tools::terminal::{ProcessBackend, ProcessHandler, TerminalHandler};
pub use tools::todo::{TodoBackend, TodoHandler};
pub use tools::tool_search::{
    assemble_tool_schemas, classify_tools, is_bridge, AssemblyResult,
    ToolCallHandler, ToolDescribeHandler, ToolSearchHandler,
};
pub use tools::tts::{TextToSpeechHandler, TtsBackend};
pub use tools::url_safety::{TirithAction, TirithCheckResult, TirithConfig, TirithRule, TirithSecurity, UrlSafetyHandler};
pub use tools::vision::{VisionAnalyzeHandler, VisionBackend};
pub use tools::web::{WebExtractBackend, WebExtractHandler, WebSearchBackend, WebSearchHandler};

// Re-export real backend implementations
pub use backends::browser::{AutoBrowserBackend, CamoFoxBrowserBackend, CdpBrowserBackend};
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
pub use backends::vision::OpenAiVisionBackend;
pub use backends::web::{BaiduHtmlSearchBackend, SimpleExtractBackend};

// Re-export builtin registration helper
pub use register_builtins::register_builtin_tools;

// Re-export core types needed by consumers
pub use hermix_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};
