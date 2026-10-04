use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

use crate::errors::{AgentError, GatewayError, ToolError};
use crate::seams::ToolOutput;
use crate::tool_schema::ToolSchema;
use crate::types::{CommandOutput, LlmResponse, Message, PtySessionInfo, PtySessionOutput, Skill, SkillMeta, StreamChunk};
use crate::ApprovalOutcome;

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
    ///
    /// 遗留的字符串返回值路径。新工具应直接实现 [`execute_structured`]；
    /// 未迁移的工具通过默认实现自动获得结构化输出，因此这里暂不删除。
    async fn execute(&self, params: Value) -> Result<String, ToolError>;

    /// Return the schema describing this tool's parameters.
    fn schema(&self) -> ToolSchema;

    /// 结构化返回值：文本与二进制产物分开，避免靠字符串猜内容类型。
    /// 内核与调度路径一律调这个方法，不调 `execute`。
    async fn execute_structured(&self, params: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(self.execute(params).await?))
    }
}

// ---------------------------------------------------------------------------
// PlatformSender
// ---------------------------------------------------------------------------

/// Abstraction for sending messages to external platforms.
/// Implemented by gateway adapters. CronRunner and other components use this
/// to deliver results to platforms without depending directly on hermix-gateway.
#[async_trait]
pub trait PlatformSender: Send + Sync {
    /// Send a text message to a platform chat.
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

    // -----------------------------------------------------------------
    // Persistent terminal sessions (ConPTY on Windows). Optional
    // capability: default implementations return "not supported" so
    // backends without session support (docker, ssh, …) need no change.
    // -----------------------------------------------------------------

    /// Spawn a persistent shell session and wait until it is ready to
    /// accept commands. Returns the `session_id` used by the other
    /// session methods.
    async fn pty_create_session(&self, workdir: Option<&str>) -> Result<String, AgentError> {
        let _ = workdir;
        Err(AgentError::Io(
            "persistent terminal sessions are not supported by this backend".to_string(),
        ))
    }

    /// Submit input to a session and read the newly produced output.
    /// `submit` appends a newline (`false` sends raw fragments such as
    /// REPL continuation lines). `read_budget_ms` caps how long the
    /// call waits for the command to settle; completion detection is
    /// heuristic (prompt marker + output silence + child-process check).
    async fn pty_send_session(
        &self,
        session_id: &str,
        input: &str,
        submit: bool,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        let _ = (session_id, input, submit, read_budget_ms);
        Err(AgentError::Io(
            "persistent terminal sessions are not supported by this backend".to_string(),
        ))
    }

    /// Read pending output from a session without submitting input.
    async fn pty_read_session(
        &self,
        session_id: &str,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        let _ = (session_id, read_budget_ms);
        Err(AgentError::Io(
            "persistent terminal sessions are not supported by this backend".to_string(),
        ))
    }

    /// List all live sessions tracked by this backend.
    async fn pty_list_sessions(&self) -> Vec<PtySessionInfo> {
        Vec::new()
    }

    /// Kill and forget a session. Unknown ids are a no-op error.
    async fn pty_close_session(&self, session_id: &str) -> Result<(), AgentError> {
        let _ = session_id;
        Err(AgentError::Io(
            "persistent terminal sessions are not supported by this backend".to_string(),
        ))
    }
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
/// - `Default` — risk-bearing tool calls (file write, shell command, network mutation)
/// need explicit user confirmation before dispatch. The agent emits a
/// `StreamEvent::RequestApproval` and blocks until a decision arrives.
/// - `Full` — no confirmation gate; all tools may be dispatched immediately.
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
/// - `Safe` — read-only, no observable side effect. Always dispatched.
/// - `Caution` — may write local files / make outbound network calls.
/// Reversible. Asks for confirmation by default.
/// - `Dangerous` — destructive or hard-to-reverse actions (delete files,
/// run shell commands with side effects, push to remote, etc.). Always
/// asks for confirmation in `Default` mode and is the strongest gate.
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

/// A single image for vision-enabled LLM calls .
/// `base64` is the raw base64 string (no data URI prefix); the agent
/// wraps it as `data:<mime>;base64,<...>` when building the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentImage {
    pub base64: String,
    pub mime_type: String,
}

/// Where an externally-triggered turn came from, so the delivery can be
/// labelled and audited after the fact.
///
/// Carried on [`AgentOverrides::external_delivery`]. `hermix-core` stays
/// free of any `hermix-automation` dependency, so the automation id
/// rides as a plain string rather than a typed reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalDelivery {
    /// Human-readable source label, shown as the message group header
    /// (e.g. an automation's name). Never shown to the model.
    pub source_label: String,
    /// The originating run's id, for linking a delivered message back to
    /// its `automation_runs` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Why this result could not land in its anchor conversation and fell
    /// back to a standalone session (guardrail cap hit, too many
    /// consecutive failures, anchor session gone).
    ///
    /// A fallback that is not explained reads as "the result vanished".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Optional overrides for a single agent request.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentOverrides {
    pub model: Option<String>,
    /// 显式 provider 名 (如 "openai" / "custom-agnes")。model 已含
    /// "provider:model" 前缀时忽略本字段 (前缀更具体); 仅当 model 无
    /// 前缀时用于补全。避免前端每次都要拼前缀。
    #[serde(default)]
    pub provider: Option<String>,
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
    /// Scoped memory directory for this turn. When `Some(dir)`, the
    /// `memory` tool and the memory prompt/prefetch injections use `dir`
    /// instead of the global `<hermix_home>/memories`, enabling per-room
    /// (group chat) memory isolation so a group chat's manager/members
    /// cannot read memories stored during a private 1:1 conversation.
    /// Private chats resolve their own session-scoped directory inside
    /// `LocalAgentService` and ignore this field.
    #[serde(default)]
    pub memory_dir: Option<String>,
    /// Permission policy for risk-bearing tool calls in this turn.
    #[serde(default)]
    pub permission: Option<PermissionLevel>,
    /// Optional images for vision-enabled models (多图支持).
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
    /// Workbench bound to this session (`SendMessageRequest.workbench_id`).
    /// `Some` makes the agent inject the `workbench.context` direction and
    /// pin the `workbench` skill into the system prompt every turn —
    /// ordinary chats (None) never see either. See
    /// WORKBENCH_CHAT_REDESIGN_PLAN B / MODULE_DESIGN §13 layer 3.
    #[serde(default)]
    pub workbench_id: Option<String>,
    /// L1 domain-mode pin (`code|research|write|operate|chat` or `"auto"`).
    /// `None` = auto-detect. Core can't name the intelligence `DomainMode`
    /// type (would be a dep cycle), so it rides as a string; the agent crate
    /// parses it in `apply_overrides`.
    #[serde(default)]
    pub domain_override: Option<String>,
    /// Toolset group name (registry tag, e.g. `office`) brought by the
    /// hired expert. The agent unions its members INTO the per-turn
    /// allow-list — an expert can add tools but never revoke the floor
    /// or the user's enabled set (`allowed` is a filter, widening it
    /// only grants). Unknown group = empty members = no-op.
    #[serde(default)]
    pub expert_toolset: Option<String>,
    /// Absolute paths of the non-image documents the user attached this
    /// turn. The server persists `SendMessageRequest.attached_files`
    /// bytes under `<hermix_home>/attachments/` before the turn starts;
    /// the agent loop appends a short note listing them to the user
    /// message so the LLM reads them with `read_file` / `read_pdf`.
    #[serde(default)]
    pub attached_file_paths: Option<Vec<String>>,
    /// Set when this turn was initiated by an external scheduler (an
    /// automation run) rather than by the user typing.
    ///
    /// It flips how this turn's two messages are **persisted**: the
    /// prompt and the reply are both stored under
    /// [`MessageRole::Automation`] instead of `user` / `assistant`.
    /// Without it an automation run leaves a fabricated "user asked
    /// this" line in the conversation, and the model — seeing it on the
    /// next turn — tries to continue answering a question nobody asked.
    ///
    /// It does not change what the agent *sees this turn*: the prompt
    /// still arrives as a real user turn, because the run has to be able
    /// to do its job. The filter only applies to what survives to the
    /// next turn.
    ///
    /// `None` for every interactive chat, so this stays a pure opt-in
    /// and ordinary conversations are unaffected.
    #[serde(default)]
    pub external_delivery: Option<ExternalDelivery>,
    /// Callback to register an approval request with the WS handler.
    /// Arguments: `(session_id, request_id)` →
    /// `oneshot::Receiver<ApprovalOutcome>`. The receiver resolves when
    /// the client sends an `ApprovalResponse`, the registry timeout
    /// fires, or the connection drops — each surfaced as a distinct
    /// `ApprovalOutcome` variant so the agent can tell "user denied"
    /// apart from "timed out" / "channel closed" (A+C product fix,
    /// ).
    #[serde(skip)]
    pub approval_requester:
        Option<Arc<dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome> + Send + Sync>>,
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
    /// The HTTP server wires this to
    /// `UserAllowlist::matches(command)` (loaded from
    /// `$HERMIX_HOME/approval_allowlist.json`) plus a per-session
    /// in-memory bypass set. The agent crate has no direct
    /// dependency on the server or the allowlist module — the
    /// callback is the integration seam.
    #[serde(skip)]
    pub pre_approval_checker:
        Option<Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>>,
    /// What to do with a Dangerous tool when this request wired no
    /// approval channel. Defaults to `Deny` (fail closed) — callers
    /// that genuinely have no remote attack surface for the decision
    /// (local stdio session, unit test) must opt in to `Allow`
    /// explicitly rather than relying on an implicit fallback.
    ///
    /// This is `#[serde(skip)]` with a `Default` of `Deny` on purpose:
    /// any existing or future caller that forgets to set it gets the
    /// safe behavior, and `Option<ApprovalFallback>` on the wire
    /// resolves `None` → `Deny` as well.
    #[serde(skip)]
    pub approval_fallback: Option<crate::approval::ApprovalFallback>,
    /// H3: Called when prefetcher resolves with an intent prediction.
    /// Payload is a JSON string: `{ intent, confidence, results }`.
    #[serde(skip)]
    pub on_prefetch_intent: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// (type-B root-cause): Soft-shutdown signal for the
    /// agent task. The server fires `notify_one()` when the WebSocket
    /// disconnects mid-turn (or on user retry). The agent loop's
    /// pre-tool-dispatch resolution races this against the approval
    /// oneshot via `tokio::select!`, so a stranded `rx.await` wakes
    /// up as `ChannelClosed` and the loop can self-drain (next
    /// `collect_stream_llm_response` honors `stream_first_chunk_timeout_secs`)
    /// instead of dying mid-await with a half-initialized context.
    /// The server still owns the `JoinHandle` and may `abort()` it
    /// if the soft drain doesn't finish in time.
    #[serde(skip)]
    pub shutdown: Option<Arc<Notify>>,
}

impl std::fmt::Debug for AgentOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOverrides")
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("personality", &self.personality)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("workspace", &self.workspace)
            .field("permission", &self.permission)
            .field("image", &self.image)
            .field("image_mime_type", &self.image_mime_type)
            .field("skills", &self.skills)
            .field("workbench_id", &self.workbench_id)
            .field("domain_override", &self.domain_override)
            .field("expert_toolset", &self.expert_toolset)
            .field("attached_file_paths", &self.attached_file_paths)
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
            && self.provider == other.provider
            && self.personality == other.personality
            && self.system_prompt == other.system_prompt
            && self.tools == other.tools
            && self.workspace == other.workspace
            && self.permission == other.permission
            && self.image == other.image
            && self.image_mime_type == other.image_mime_type
            && self.skills == other.skills
            && self.workbench_id == other.workbench_id
            && self.domain_override == other.domain_override
            && self.expert_toolset == other.expert_toolset
            && self.attached_file_paths == other.attached_file_paths
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
    /// inspect the persisted message to dispatch them
    /// - `"length"` — the response was cut off by the provider's token cap
    /// (a truncation, not a normal completion)
    /// - `"interrupted"` — the user cancelled the stream
    /// - `"error"` — the loop stopped on an LLM or transport failure. The
    /// turn did **not** succeed; pair with `last_error` for the cause and
    /// with `total_turns` to learn whether anything ran before it stopped.
    /// This is a normal `Ok` result, not an `Err`: the transcript is still
    /// persisted so the user can see what was attempted. Do not treat
    /// `Ok(..)` alone as success — check this field.
    /// - `None` — the agent did not surface a reason (e.g. an error path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Raw backend error text captured when the loop failed (e.g. invalid
    /// tool-call arguments emitted by the model that the provider rejected
    /// with a 400). Surfaced to the frontend as the error card's
    /// "技术详情" (technical detail) subtext — the user-facing `message`
    /// stays in Chinese, this holds the raw English for diagnosis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Whether the turn ended due to an interruption (user cancel, fatal
    /// invalid-tool exhaustion, backend failure) rather than a natural
    /// completion. Surfaced so the transport layer can mark the `Done`
    /// event outcome as `interrupted` and the frontend keeps the session
    /// alive instead of tearing down the socket.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    /// How many LLM round-trips the agent attempted this turn.
    ///
    /// `AgentLoop` bumps the counter *before* each LLM call, so the value
    /// answers "how far did we get": `1` means the very first call failed
    /// and **no tool ever ran**, `>= 2` means at least one successful
    /// response came back, which may have driven tool calls.
    ///
    /// This is the only signal that separates "the request never landed"
    /// from "the agent worked and then failed" — `AgentLoop::run()` returns
    /// `Ok` for both (an LLM failure is converted to a graceful interrupt
    /// so the transcript gets persisted), and `last_error` carries the
    /// error text with no indication of how far the loop had progressed.
    /// Callers that may **retry** the turn need that distinction: replaying
    /// a turn that already ran tools would repeat their side effects.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub total_turns: u32,
}

/// serde helper: skip serializing a zero counter.
#[allow(dead_code)]
pub(crate) fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// 主对话轮次 `TaskRecord` 的 id 前缀。
///
/// **跨 crate 契约**：hermix-agent 用它造 id（写记录），hermix-server 用它
/// 过滤 id（读终态）。`task_records` 表里混着 `delegate_task` 的 sub-agent
/// 记录，只按 `conversation_id` 过滤会把它们一起捞出来，所以必须靠这个
/// 前缀区分「对话轮次」与「子任务」。
pub const TURN_ID_PREFIX: &str = "turn-";

/// 判据本体：这一轮是被**用户**停掉的吗？
///
/// `AgentReply`（流式路径）与 `AgentResult`（同步路径）字段同构，判据必须
/// 只有一份，所以放在自由函数里、由两者各自包装。故意收得窄：单看
/// `interrupted` 会把致命工具参数耗尽、后端 5xx 也算进去（见字段注释），
/// 那等于让用户背一个 5xx 的锅。必须配对 `finish_reason` 这个文档化的
/// 判别字段：用户停 = `interrupted` + `"interrupted"`；后端失败 = `"error"`。
pub fn is_user_interrupted(interrupted: bool, finish_reason: Option<&str>) -> bool {
    interrupted && finish_reason == Some("interrupted")
}

impl AgentReply {
    /// 见 [`is_user_interrupted`]。
    pub fn was_user_interrupted(&self) -> bool {
        is_user_interrupted(self.interrupted, self.finish_reason.as_deref())
    }
}

/// serde helper: skip serializing `interrupted` when it is `false`.
#[allow(dead_code)]
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
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

    /// Interrupt all active agent tasks so they gracefully stop,
    /// persist their partial output, and return. Called by the
    /// server's shutdown path before the process exits — without
    /// this, spawned agent tasks are dropped mid-execution and
    /// their accumulated streaming output is lost (the session
    /// reloads empty on restart). Default is a no-op for services
    /// that don't track active tasks.
    fn interrupt_all(&self) {}

    /// Interrupt **one** session's in-flight turn, identified by its
    /// session id, leaving every other running turn alone. Returns
    /// `true` when a live turn was found and signalled, `false` when
    /// nothing was running for that id.
    ///
    /// This is what remote channels need (`/stop` from IM, the mobile
    /// client's task screen): `interrupt_all` is a *shutdown* primitive,
    /// so using it there would let one user stop another user's work.
    /// Default is `false` for services that don't track turns per session.
    fn interrupt_session(&self, _sid: &str) -> bool {
        false
    }

    /// Inject mid-run user guidance into **one** session's in-flight turn
    /// (IM `/queue steer`). Returns `false` when no such turn is running
    /// or the text is empty.
    ///
    /// This is a different lever from `interrupt_session`: the turn keeps
    /// going and the guidance lands in the next tool result, so the user
    /// can correct course without losing the work done so far.
    fn steer_session(&self, _sid: &str, _text: &str) -> bool {
        false
    }

    /// Fire-and-forget cold-start warmup for a specific model (e.g. when
    /// the UI switches the active model away from the configured default).
    /// Primes the provider's HTTP keep-alive pool so the first chat on
    /// that model doesn't pay the 30–60s cold-start cost. Default is a
    /// no-op for remote / mocked services.
    fn warm_up_provider(&self, _model: &str) {}
}

// ---------------------------------------------------------------------------
// ContextEngine
// ---------------------------------------------------------------------------

/// Trait for context window management engines.
/// Implementations compress conversation history when approaching the
/// model's context limit. The default implementation uses LLM-powered
/// summarization; alternative implementations could use sliding windows,
/// token counting, or other strategies.
/// This trait enables pluggable context management — the agent loop
/// can swap compression strategies without changing its core logic.
#[async_trait]
pub trait CoreContextEngine: Send + Sync {
    /// Check if the current context should be compressed.
    /// `current_tokens` is the estimated token count of the current
    /// prompt. Returns `true` if compression should be triggered.
    fn should_compress(&self, current_tokens: u64) -> bool;

    /// Compress the conversation history.
    /// Takes the full message list and returns a compressed version.
    /// The implementation decides what to keep, summarize, or drop.
    async fn compress(&mut self, messages: Vec<Message>) -> Vec<Message>;

    /// Compress without blocking on the LLM summariser.
    /// Used when context pressure is moderate (e.g. 50–85% of the
    /// window) and blocking one turn on an LLM summary call is not
    /// yet justified. The fast path still runs the archive + dedup
    /// + score-drop passes, but replaces the LLM summary with a
    /// static fallback marker — the original turns are archived to
    /// disk first, so nothing is lost, the LLM just doesn't get a
    /// semantic handoff for the removed turns.
    /// When pressure crosses the high-water mark (e.g. >= 85%) the
    /// caller should switch back to [`Self::compress`] so the LLM
    /// gets a real summary before the window is at risk of
    /// overflowing.
    /// Default falls back to [`Self::compress`] for engines that
    /// don't distinguish (e.g. `NoOpCompressionEngine`).
    async fn compress_fast(&mut self, messages: Vec<Message>) -> Vec<Message> {
        self.compress(messages).await
    }

    /// Notify the engine about session reset (/new, /reset).
    /// Implementations should clear per-session state.
    fn on_session_reset(&mut self) {}

    /// Notify the engine about session end (CLI exit, gateway expiry).
    /// Implementations should clear per-session state that shouldn't
    /// survive across sessions.
    fn on_session_end(&mut self, _session_id: &str) {}
}

/// (, Hermes 对标): a richer trait layered on top
/// of [`CoreContextEngine`] that exposes the *post-compression*
/// bookkeeping the agent loop needs to drive the
/// `CompactionStatusBanner`, persist the snapshot, and feed the
/// breaker state machine. Built-in [`ContextCompressor`]
/// implements this; a plugin can replace compression by
/// supplying its own `CompressionEngine` implementation and
/// handing it to the agent loop via
/// `AgentLoop::set_compression_engine`.
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

    /// same as [`Self::breaker_snapshot`] but lets the
    /// caller stamp the compression `phase` explicitly so the
    /// front-end `CompactionStatusBanner` can render the
    /// "starting / in_progress / complete / failed" transitions.
    /// Default impl is `breaker_snapshot(...)` with `phase` forced
    /// to `"idle"` so existing plugin implementors continue to
    /// compile without changes. The agent loop's built-in
    /// `ContextCompressor` overrides this to forward the phase
    /// into the snapshot.
    fn breaker_snapshot_with_phase(
        &mut self,
        current_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        failure_reason: Option<String>,
        phase: &str,
    ) -> CompressionEngineSnapshot {
        let _ = phase;
        self.breaker_snapshot(current_tokens, threshold_tokens, max_tokens, failure_reason)
    }

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
    /// Process phase of the most recent compression
    /// attempt, surfaced to the front-end `CompactionStatusBanner`
    /// so it can render "正在压缩中…" / "压缩完成" / "压缩失败"
    /// transitions instead of jumping straight from "即将触发自动
    /// 压缩" to the post-attempt result. `"idle"` is the default
    /// for snapshots produced outside an active attempt (e.g. a
    /// periodic heartbeat). Older snapshots that don't carry the
    /// field are decoded as `"idle"` — additive change, no wire-
    /// format break.
    #[serde(default = "default_phase_idle")]
    pub phase: String,
}

#[allow(dead_code)]
fn default_phase_idle() -> String {
    "idle".to_string()
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
    /// number of adjacent user-text dedups
    /// in the most recent compress() call (Pass 0). 0 means none.
    pub user_dedup_count: u64,
    /// number of messages preserved by
    /// the pin filter in the most recent compress() call.
    pub pin_count: u64,
    /// number of messages dropped by
    /// `TokenScorer` because the score was below the configured
    /// `drop_score_threshold`.
    pub dropped_count: u64,
    /// what triggered the compression —
    /// `"threshold"`, `"explicit"`, etc. Empty means unknown.
    /// Boxed String keeps the struct layout-compatible for
    /// existing fields even when serde renames this on the wire.
    pub trigger_reason: String,
}
