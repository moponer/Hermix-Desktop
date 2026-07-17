use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{AgentError, GatewayError, ToolError};
use crate::tool_schema::ToolSchema;
use crate::types::{CommandOutput, LlmResponse, Message, Skill, SkillMeta, StreamChunk};

// ---------------------------------------------------------------------------
// LlmProvider
// ---------------------------------------------------------------------------

/// Trait for LLM provider backends.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Perform a single chat completion request.
    async fn chat_completion(
        &self,
        messages: &[crate::types::Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError>;

    /// Perform a streaming chat completion, returning a stream of chunks.
    fn chat_completion_stream(
        &self,
        messages: &[crate::types::Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>>;
}

// ---------------------------------------------------------------------------
// ToolHandler
// ---------------------------------------------------------------------------

/// Trait for tool execution handlers.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Execute the tool with the given parameters.
    async fn execute(&self, params: Value) -> Result<String, ToolError>;

    /// Return the schema describing this tool's parameters.
    fn schema(&self) -> ToolSchema;
}

// ---------------------------------------------------------------------------
// PlatformSender
// ---------------------------------------------------------------------------

/// Abstraction for sending messages to external platforms.
///
/// Implemented by gateway adapters. CronRunner and other components use this
/// to deliver results to platforms without depending directly on hermix-gateway.
#[async_trait]
pub trait PlatformSender: Send + Sync {
    /// Send a text message to a platform chat.
    ///
    /// `platform` is the adapter name (e.g. "telegram", "discord", "dingtalk").
    /// `chat_id` is the platform-specific chat/channel identifier.
    async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// PlatformAdapter
// ---------------------------------------------------------------------------

/// Parse mode for platform messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseMode {
    Plain,
    Markdown,
    Html,
}

/// Trait for platform communication adapters (Telegram, Discord, etc.).
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Start the platform adapter (connect, begin listening).
    async fn start(&self) -> Result<(), GatewayError>;

    /// Stop the platform adapter gracefully.
    async fn stop(&self) -> Result<(), GatewayError>;

    /// Send a text message to a chat.
    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError>;

    /// Edit an existing message.
    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError>;

    /// Send a file to a chat with an optional caption.
    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError>;

    /// Check whether the adapter is currently running.
    fn is_running(&self) -> bool;

    /// Return the name of this platform (e.g. "telegram", "discord").
    fn platform_name(&self) -> &str;

    /// Periodic maintenance: prune token caches, dedup maps, etc.
    ///
    /// Default is a no-op. Adapters with long-lived in-memory caches should
    /// override this so gateway cleanup watchers can reclaim memory.
    async fn maintenance_prune(&self) {}
}

// ---------------------------------------------------------------------------
// TerminalBackend
// ---------------------------------------------------------------------------

/// Trait for terminal / shell backend implementations.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Execute a command in the terminal.
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError>;

    /// Read a file's contents (with optional offset and line limit).
    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError>;

    /// Write content to a file.
    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError>;

    /// Check whether a file exists at the given path.
    async fn file_exists(&self, path: &str) -> Result<bool, AgentError>;
}

// ---------------------------------------------------------------------------
// MemoryProvider
// ---------------------------------------------------------------------------

/// Trait for key-value memory storage backends.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Save a value under a namespace + key.
    async fn save(&self, namespace: &str, key: &str, value: &str) -> Result<(), AgentError>;

    /// Load a value from a namespace + key.
    async fn load(&self, namespace: &str, key: &str) -> Result<Option<String>, AgentError>;

    /// List all namespaces.
    async fn list_namespaces(&self) -> Result<Vec<String>, AgentError>;

    /// Delete a value from a namespace + key.
    async fn delete(&self, namespace: &str, key: &str) -> Result<(), AgentError>;
}

// ---------------------------------------------------------------------------
// SkillProvider
// ---------------------------------------------------------------------------

/// Trait for skill management backends.
#[async_trait]
pub trait SkillProvider: Send + Sync {
    /// Create a new skill.
    async fn create_skill(
        &self,
        name: &str,
        content: &str,
        category: Option<&str>,
    ) -> Result<Skill, AgentError>;

    /// Get a skill by name.
    async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError>;

    /// List all skills with metadata.
    async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError>;

    /// Update an existing skill's content.
    async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError>;

    /// Delete a skill by name.
    async fn delete_skill(&self, name: &str) -> Result<(), AgentError>;
}

// ---------------------------------------------------------------------------
// AgentOverrides / AgentReply
// ---------------------------------------------------------------------------

/// Permission policy for a single agent request.
///
/// - `Default` — risk-bearing tool calls (file write, shell command, network mutation)
///   need explicit user confirmation before dispatch. The agent emits a
///   `StreamEvent::RequestApproval` and blocks until a decision arrives.
/// - `Full` — no confirmation gate; all tools may be dispatched immediately.
///
/// Frontend default is `Default`. `Full` is opt-in per-session and should be
/// surfaced with a clear "skip safety prompts" affordance in the UI.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    #[default]
    Default,
    Full,
}

/// Risk classification for a single tool. Used by the pre-execution
/// approval hook to decide whether a tool call needs user confirmation
/// when the session is in [`PermissionLevel::Default`] mode.
///
/// - `Safe` — read-only, no observable side effect. Always dispatched.
/// - `Caution` — may write local files / make outbound network calls.
///   Reversible. Asks for confirmation by default.
/// - `Dangerous` — destructive or hard-to-reverse actions (delete files,
///   run shell commands with side effects, push to remote, etc.). Always
///   asks for confirmation in `Default` mode and is the strongest gate.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    #[default]
    Caution,
    Dangerous,
}

impl RiskLevel {
    /// String identifier used in the wire format (lowercase snake_case).
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "safe",
            RiskLevel::Caution => "caution",
            RiskLevel::Dangerous => "dangerous",
        }
    }
}

// ---------------------------------------------------------------------------
// PermissionMode
// ---------------------------------------------------------------------------

/// Permission level for tool execution (inspired by claw-code design).
///
/// Determines what operations a tool is allowed to perform:
/// - `ReadOnly`: Only read operations (file read, search, web fetch)
/// - `WorkspaceWrite`: Read + write within workspace boundaries
/// - `DangerFullAccess`: Full access including shell commands, network, etc.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Read-only operations (file read, search, web fetch).
    ReadOnly,
    /// Read + write within workspace boundaries.
    #[default]
    WorkspaceWrite,
    /// Full access including shell commands, network, etc.
    DangerFullAccess,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::ReadOnly => "read-only",
            PermissionMode::WorkspaceWrite => "workspace-write",
            PermissionMode::DangerFullAccess => "danger-full-access",
        }
    }

    /// Check if this mode allows the given operation.
    pub fn allows(&self, required: &PermissionMode) -> bool {
        match self {
            PermissionMode::DangerFullAccess => true,
            PermissionMode::WorkspaceWrite => matches!(required, PermissionMode::ReadOnly | PermissionMode::WorkspaceWrite),
            PermissionMode::ReadOnly => matches!(required, PermissionMode::ReadOnly),
        }
    }
}

/// A single image for vision-enabled LLM calls (2026-06-25).
/// `base64` is the raw base64 string (no data URI prefix); the agent
/// wraps it as `data:<mime>;base64,<...>` when building the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentImage {
    pub base64: String,
    pub mime_type: String,
}

/// Optional overrides for a single agent request.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentOverrides {
    pub model: Option<String>,
    pub personality: Option<String>,
    /// Fully replace the system prompt for this turn. The agent uses this
    /// verbatim in the prompt assembly, after the SOUL/personality blocks.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Whitelist of tool names that may be invoked during this turn. When
    /// `None` (the default), the agent's normal tool registry is used. When
    /// `Some`, the bridge layer filters the registry down to this set.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Canonical working directory for this turn. Tool dispatchers (terminal,
    /// file tools) constrain all relative path resolution against this root.
    /// When `None`, no workspace boundary is enforced.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Permission policy for risk-bearing tool calls in this turn.
    #[serde(default)]
    pub permission: Option<PermissionLevel>,
    /// Optional images for vision-enabled models (2026-06-25: 多图支持).
    /// Each entry carries base64-encoded data + MIME type. The agent
    /// prepends image content parts so the LLM can "see" them.
    #[serde(default)]
    pub images: Option<Vec<AgentImage>>,
    /// Legacy single-image field (kept for backward compat with older
    /// clients that send `image` instead of `images`).
    #[serde(default)]
    pub image: Option<String>,
    /// MIME type for the legacy single-image field.
    #[serde(default)]
    pub image_mime_type: Option<String>,
    /// Ephemeral per-turn skill set. Names of skills to surface in the
    /// system prompt on top of the server-side enabled set. The agent
    /// loop reads them via `agent_loop::skills_system_prompt` and renders
    /// their `SKILL.md` content into the prompt. The server-side
    /// enabled-skills list is still the floor — anything the user picks
    /// here is *added*, never removed.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// Callback to register an approval request with the WS handler.
    /// Arguments: `(session_id, request_id)` → `oneshot::Receiver<bool>`.
    /// The receiver resolves when the client sends an `ApprovalResponse`.
    #[serde(skip)]
    pub approval_requester:
        Option<Arc<dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<bool> + Send + Sync>>,
    /// Callback to send a non-text event (e.g. `RequestApproval`) to the WS client.
    /// The argument is a `serde_json::Value` representing the event payload
    /// (without the `WsEnvelope` wrapper — the caller adds version/trace_id).
    #[serde(skip)]
    pub on_event: Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>>,
    /// Pre-approval shortcut. When a tool call would otherwise send a
    /// `RequestApproval` to the WS client, the agent first consults
    /// this callback with `(tool_name, raw_params)`. If it returns
    /// `true`, the call is auto-approved **without** a WS round-trip
    /// — the user previously picked "Always Allow" (persistent) or
    /// "Allow for this session" for the same shape. If `None`, the
    /// agent always goes through the WS prompt.
    ///
    /// The HTTP server wires this to
    /// `UserAllowlist::matches(command)` (loaded from
    /// `$HERMIX_HOME/approval_allowlist.json`) plus a per-session
    /// in-memory bypass set. The agent crate has no direct
    /// dependency on the server or the allowlist module — the
    /// callback is the integration seam.
    #[serde(skip)]
    pub pre_approval_checker:
        Option<Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>>,
    /// H3: Called when prefetcher resolves with an intent prediction.
    /// Payload is a JSON string: `{ intent, confidence, results }`.
    #[serde(skip)]
    pub on_prefetch_intent: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl std::fmt::Debug for AgentOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOverrides")
            .field("model", &self.model)
            .field("personality", &self.personality)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("workspace", &self.workspace)
            .field("permission", &self.permission)
            .field("image", &self.image)
            .field("image_mime_type", &self.image_mime_type)
            .field("skills", &self.skills)
            .field(
                "approval_requester",
                &self.approval_requester.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_event",
                &self.on_event.as_ref().map(|_| "<callback>"),
            )
            .field(
                "pre_approval_checker",
                &self.pre_approval_checker.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_prefetch_intent",
                &self.on_prefetch_intent.as_ref().map(|_| "<callback>"),
            )
            .finish()
    }
}

impl PartialEq for AgentOverrides {
    fn eq(&self, other: &Self) -> bool {
        // Compare all data fields; callback identity is not meaningful for
        // equality (two differently-constructed callbacks represent the same
        // request from the bus's perspective).
        self.model == other.model
            && self.personality == other.personality
            && self.system_prompt == other.system_prompt
            && self.tools == other.tools
            && self.workspace == other.workspace
            && self.permission == other.permission
            && self.image == other.image
            && self.image_mime_type == other.image_mime_type
            && self.skills == other.skills
    }
}

impl Eq for AgentOverrides {}

/// Reply from an agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReply {
    /// The assistant's text response.
    pub text: String,
    /// Total message count in the session after this exchange.
    pub message_count: usize,
    /// Why the stream ended. Possible values:
    /// - `"stop"` — the model emitted a natural `stop` and the turn is done
    /// - `"tool_calls"` — the model returned tool calls; the caller should
    ///   inspect the persisted message to dispatch them
    /// - `"length"` — the response was cut off by the provider's token cap
    ///   (a truncation, not a normal completion)
    /// - `"interrupted"` — the user cancelled the stream
    /// - `None` — the agent did not surface a reason (e.g. an error path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentService
// ---------------------------------------------------------------------------

/// Abstraction for agent execution — callers are agnostic about
/// whether the agent runs in-process or remotely.
#[async_trait]
pub trait AgentService: Send + Sync {
    /// Send a message to a session and get the reply.
    async fn send_message(
        &self,
        session_id: &str,
        text: &str,
        overrides: AgentOverrides,
    ) -> Result<AgentReply, AgentError>;

    /// Send a message and stream back chunks followed by a final reply.
    async fn send_message_stream(
        &self,
        session_id: &str,
        text: &str,
        overrides: AgentOverrides,
        on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync>,
    ) -> Result<AgentReply, AgentError>;

    /// Get all messages for a session.
    async fn get_session_messages(&self, session_id: &str) -> Result<Vec<Message>, AgentError>;

    /// Clear a session's message history.
    async fn reset_session(&self, session_id: &str) -> Result<(), AgentError>;

    /// Replace the live config used by this service. Called by
    /// `POST /api/config/auto-correct` (which the Settings UI fires
    /// after a new key is saved) and by the equivalent startup hook,
    /// so the next chat picks up the new model/provider without
    /// requiring a desktop-app restart. The default impl is a no-op
    /// for remote / mocked services that don't have a local config
    /// to refresh. The argument is a serialized config payload
    /// (typically a `GatewayConfig` round-tripped through serde) so
    /// this trait can stay free of any concrete config-crate deps.
    fn set_config(&self, _config: Value) {}

    /// Snapshot the current config as a serialized value. Used by
    /// the `/api/config/auto-correct` endpoint so it can mutate a
    /// fresh clone (preserving any in-flight overrides) rather than
    /// the startup-time `state.config` snapshot, which may be
    /// stale. Default returns `None` for services that don't
    /// maintain a live config.
    fn current_config_value(&self) -> Option<Value> {
        None
    }
}

// ---------------------------------------------------------------------------
// ContextEngine
// ---------------------------------------------------------------------------

/// Trait for context window management engines.
///
/// Implementations compress conversation history when approaching the
/// model's context limit. The default implementation uses LLM-powered
/// summarization; alternative implementations could use sliding windows,
/// token counting, or other strategies.
///
/// This trait enables pluggable context management — the agent loop
/// can swap compression strategies without changing its core logic.
#[async_trait]
pub trait CoreContextEngine: Send + Sync {
    /// Check if the current context should be compressed.
    ///
    /// `current_tokens` is the estimated token count of the current
    /// prompt. Returns `true` if compression should be triggered.
    fn should_compress(&self, current_tokens: u64) -> bool;

    /// Compress the conversation history.
    ///
    /// Takes the full message list and returns a compressed version.
    /// The implementation decides what to keep, summarize, or drop.
    async fn compress(&mut self, messages: Vec<Message>) -> Vec<Message>;

    /// Notify the engine about session reset (/new, /reset).
    /// Implementations should clear per-session state.
    fn on_session_reset(&mut self) {}

    /// Notify the engine about session end (CLI exit, gateway expiry).
    /// Implementations should clear per-session state that shouldn't
    /// survive across sessions.
    fn on_session_end(&mut self, _session_id: &str) {}
}

/// 2026-07-05 (N-03, Hermes 对标): a richer trait layered on top
/// of [`CoreContextEngine`] that exposes the *post-compression*
/// bookkeeping the agent loop needs to drive the
/// `CompactionStatusBanner`, persist the snapshot, and feed the
/// breaker state machine. Built-in [`ContextCompressor`]
/// implements this; a plugin can replace compression by
/// supplying its own `CompressionEngine` implementation and
/// handing it to the agent loop via
/// `AgentLoop::set_compression_engine`.
///
/// Anything that *only* needs the basic compress-loop semantics
/// can stay with [`CoreContextEngine`]; the agent loop reaches
/// for [`CompressionEngine`] only at the
/// `auto_compress_if_over_threshold` boundary where the
/// bookkeeping methods matter.
#[async_trait]
pub trait CompressionEngine: CoreContextEngine {
    /// Return the summary text produced by the most recent
    /// `compress()` call (so `local_agent_service` can persist
    /// it on `sessions.compressed_summary`). `None` when no
    /// compression has happened yet, or when the engine has no
    /// notion of a summary.
    fn previous_summary(&self) -> Option<&str>;

    /// Return the raw turns the most recent compression folded
    /// into the summary. The agent loop ships these to
    /// `compression_archives` so the original wording survives
    /// after `flush_messages` has wiped it from `messages`.
    fn last_archived_turns(&self) -> &[Message];

    /// Return the metrics from the most recent compression
    /// attempt (tokens_saved, summary_errors, breaker stats).
    fn last_metrics(&self) -> CompressionEngineMetrics;

    /// Take a pre-attempt snapshot of the breaker / budget
    /// state. `current_tokens` and `threshold_tokens` describe
    /// the live budget pressure; `failure_reason` is `Some`
    /// when this snapshot represents a failed attempt.
    ///
    /// Takes `&mut self` because the built-in `ContextCompressor`
    /// derives cooldown timing from `Instant::now()` relative to
    /// a stored deadline; a purely shared-receiver would force
    /// an `unsafe` shim. Plugins are free to ignore the
    /// mutability (they don't have internal cooldown clocks).
    fn breaker_snapshot(
        &mut self,
        current_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        failure_reason: Option<String>,
    ) -> CompressionEngineSnapshot;

    /// Record the outcome of a compression attempt and return
    /// the resulting breaker state. `success == false` rolls
    /// the breaker toward Open (circuit-break); `success ==
    /// true` rolls it back toward Closed.
    fn record_outcome(&mut self, success: bool) -> CompressionEngineState;
}

/// Snapshot of a single compression-attempt's breaker state.
/// Mirrors `compression::BreakerSnapshot` so the agent loop can
/// emit it via `emit_compaction_status` without coupling to the
/// built-in `ContextCompressor` type.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompressionEngineSnapshot {
    pub state: CompressionEngineState,
    pub current_tokens: u64,
    pub threshold_tokens: u64,
    pub max_tokens: u64,
    pub failure_reason: Option<String>,
}

/// Coarse breaker state. Mirrors `compression::BreakerState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CompressionEngineState {
    Closed,
    HalfOpen,
    Open,
}

/// Metrics emitted by the most recent compression attempt.
/// Mirrors the subset of `compression::CompressionMetrics` the
/// agent loop actually reads.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CompressionEngineMetrics {
    pub tokens_saved: u64,
    pub summary_errors: u64,
    pub compression_count: u64,
    /// 2026-07-17 (Phase A1): number of adjacent user-text dedups
    /// in the most recent compress() call (Pass 0). 0 means none.
    pub user_dedup_count: u64,
    /// 2026-07-17 (Phase A2): number of messages preserved by
    /// the pin filter in the most recent compress() call.
    pub pin_count: u64,
    /// 2026-07-17 (Phase B6): number of messages dropped by
    /// `TokenScorer` because the score was below the configured
    /// `drop_score_threshold`.
    pub dropped_count: u64,
    /// 2026-07-17 (Phase A4): what triggered the compression —
    /// `"threshold"`, `"explicit"`, etc. Empty means unknown.
    /// Boxed String keeps the struct layout-compatible for
    /// existing fields even when serde renames this on the wire.
    pub trigger_reason: String,
}
