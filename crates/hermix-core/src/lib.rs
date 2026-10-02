//! # hermes-core
//!
//! Foundation crate defining all shared types, traits, and error types
//! used across the hermes-agent-rust workspace.

pub mod acui;
pub mod agentspec;
pub mod approval;
pub mod errors;
pub mod proc;
pub mod tool_call_parser;
pub mod tool_call_sanitizer;
pub mod tool_schema;
pub mod traits;
pub mod types;
// Cross-platform helpers added in the Windows-first hardening pass
//. `path_security` is a no-dep stdlib module;
// `console_decode` always compiles but its GBK fallback is gated to
// Windows where CP936 / OEM encoding is the dominant mojibake source.
pub mod path_security;
pub mod console_decode;
#[cfg(windows)]
pub mod win_job;
pub mod text_utils;
pub mod plugin_abi;
pub mod runtime;
pub mod permission_profile;
// 内核接缝（C3）：内核只认这些 trait，业务只实现它们
pub mod seams;
// 来源感知权限（§5.5 第 5 层）：权限矩阵唯一权威
pub mod origin;

#[cfg(test)]
pub mod test_generators;

// Re-export all error types
pub use errors::{AgentError, ConfigError, GatewayError, ToolError};

// Re-export all core types
pub use types::{
    AgentResult, BudgetConfig, BudgetConfigError, CacheControl, CacheType, CommandOutput,
    FunctionCall, FunctionCallDelta, LlmResponse, Message, MessageRole, PtySessionInfo,
    PtySessionOutput, ReasoningContent, ReasoningFormat, Skill, SkillMeta, StreamChunk,
    StreamDelta, ToolCall, ToolCallDelta, ToolErrorRecord, ToolResult, UsageStats,
    BUDGET_CONFIG_MAX_CHARS,
};

//Re-export Orchestration Engine types
pub use types::{
    TaskArtifactType, TaskContext, TaskContextRef, TaskContextRefKind, TaskMode, TaskResultRef,
    TaskStatus,
};

// Re-export tool schema types
pub use tool_schema::{tool_schema, JsonSchema, ToolCatalogEntry, ToolSchema};

// Re-export trait definitions
pub use traits::{
    AgentService, LlmProvider, MemoryProvider, PlatformAdapter, PlatformSender, SkillProvider,
    TerminalBackend, ToolHandler,
};

// Re-export AgentService supporting types
pub use traits::{AgentOverrides, AgentReply};

// Re-export the pluggable context-compression surface ()
pub use traits::{
    CompressionEngine, CompressionEngineMetrics, CompressionEngineSnapshot,
    CompressionEngineState, CoreContextEngine,
};

// Re-export tool call parser public API
pub use tool_call_parser::{
    format_tool_calls, get_parser, parse_tool_calls, register_parser, separate_text_and_calls,
    HermesToolCallParser, ToolCallParser,
};

// Re-export the streaming tool-call XML sanitizer
pub use tool_call_sanitizer::ToolCallSanitizer;

// Re-export the kernel seams (C3)
pub use seams::{
    AttachmentKind, DirEntry, Envelope, ExecOutput, ExecWorld, Fs, FsError, Op, Sandbox, Shell,
    ShellError, ShellErrorKind, ToolAttachment, ToolOutput, Transport, TransportError, Verdict,
};
pub use origin::{Capability, Decision as OriginDecision, Origin};
pub use seams::local::{
    FixedExecWorld, InMemoryFs, LocalExecWorld, LocalFs, LocalSandbox, LocalShell, NoopTransport,
    NullShell,
};

// Re-export ACUI directive types
pub use acui::{AcuiDirective, AcuiHide, AcuiPosition, AcuiShow, AcuiState, AcuiUpdate};

// Re-export ParseMode from traits
pub use traits::ParseMode;

// Re-export ApprovalOutcome taxonomy (A+C product fix)
pub use approval::ApprovalOutcome;

// Re-export PermissionMode and RiskLevel from traits
pub use traits::{PermissionMode, RiskLevel};

//Re-export the shared agent-spec data model (Groupchat 未来④ Agent 抽象统一, )
pub use agentspec::{spec_from_contract_parts, AgentSpecData};

// Re-export text utilities
pub use text_utils::{format_token_count, is_error_json, truncate_chars};

//Re-export the declarative permission profile (P6, ) —
// lives in core so both `hermix-config::GatewayConfig` (for
// persistence) and `hermix_agent::AgentConfig` (for runtime gating)
// can embed it without a circular dependency.
pub use permission_profile::{PendingGrant, PermissionProfile};
