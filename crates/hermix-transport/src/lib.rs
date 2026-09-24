use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 传输契约（自 server/transport.rs 下沉，P0「传输两套合一」）
//
// 「客户端 ↔ 服务端」全双工通道的最小接口。handler 只依赖这三个 trait，
// 不感知底层是 WebSocket 还是 stdio 管道。拆成独立的入/出站半，是为了让
// handler 能并发读写——审批响应等 out-of-band 消息必须在推流的同时被
// 读取，否则会堆积在接收缓冲区直到超时。
// ---------------------------------------------------------------------------

pub type TransportError = Box<dyn std::error::Error + Send + Sync>;
pub type TransportResult<T> = Result<T, TransportError>;

/// 出站半：服务端 → 客户端，逐条发送 JSON 文本。
#[async_trait::async_trait]
pub trait TransportSink: Send {
    async fn send(&mut self, text: String) -> TransportResult<()>;
    /// 传输层保活探针。WS 实现发协议级 Ping 帧刷新 NAT/中间盒空闲计时器，
    /// 并通过发送失败及早发现死连接；无帧概念的传输（stdio）默认空操作。
    async fn ping(&mut self) -> TransportResult<()> {
        Ok(())
    }
    /// 正常关闭出站方向；缓冲区刷新失败即视为错误。
    async fn close(&mut self) -> TransportResult<()>;
}

/// 入站半：客户端 → 服务端，逐条读取 JSON 文本；`None` 表示对端已关闭。
#[async_trait::async_trait]
pub trait TransportSource: Send {
    async fn recv(&mut self) -> Option<String>;
}

/// 全双工传输。拆成独立的入/出站半供 handler 并发持有。
pub trait Transport: Send {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>);
}

// Re-export the core `ToolCall` shape so the wire-format
// `ProtocolMessage::tool_calls` field (and any other payload that
// round-trips a tool call) can name the type without forcing every
// consumer to import `hermix_core` directly. The `transport` crate
// already depends on `hermix_core` for shared types (role enum,
// message role); surfacing `ToolCall` here keeps the wire schema
// self-contained.
pub use hermix_core::ToolCall;
// re-export `ExecutionTimelineEvent` alongside
// `ToolCall` so the wire-format `ProtocolMessage::execution_timeline`
// field can name the type without forcing consumers to import
// `hermix_core::types` directly. Mirrors the rationale for `ToolCall`
// above — keeping the wire schema self-contained.
pub use hermix_core::types::ExecutionTimelineEvent;

pub type SessionId = String;
pub type MessageId = String;

/// A single prefetch result entry sent over the wire.
/// Lightweight version of `hermix_intelligence::PrefetchResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrefetchResultEntry {
    /// Tool name (e.g. "weather", "read_file").
    pub tool: String,
    /// Arguments passed to the tool (JSON-serializable).
    pub args: serde_json::Value,
    /// Tool output (opaque string).
    pub output: String,
    /// Whether the tool execution errored.
    pub errored: bool,
    /// Execution time in milliseconds.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
    /// UI-only: an inline approval card sandwiched between assistant
    /// messages. Never sent to the LLM — the server filters these out
    /// when constructing conversation context (see
    /// `SessionPersistence::load_session`).
    Approval,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolMessage {
    pub id: MessageId,
    pub role: Role,
    pub content: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Persisted tool-calls payload from the assistant message —
    /// mirrors `Message::tool_calls`. The desktop client uses this
    /// to rebuild the inline tool cards (with the original tool
    /// name + arguments) on session reload. Populated for
    /// `role == "assistant"` only; `None` for user / system /
    /// tool / approval rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<crate::ToolCall>>,
    /// Tool name on `role == "tool"` rows — mirrors
    /// `Message::name`. The desktop client uses this to label
    /// reloaded tool messages (e.g. "terminal" instead of the
    /// generic "tool" fallback). `None` for other roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool arguments (JSON string) on `role == "tool"` rows —
    /// reconstructed from the linked assistant message's
    /// `tool_calls[].function.arguments` when the session is
    /// reloaded. Without this, the desktop client renders every
    /// reloaded tool message with a generic "调用工具" verb and
    /// no primary-arg preview. `None` for other roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<String>,
    /// Free-form JSON for non-message rows (e.g. approval cards carry
    /// `request_id` / `tool` / `args` / `risk` / `decision` / `decided_at`).
    /// Always `None` for user / assistant / system / tool messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    /// The model's raw reasoning (thinking monologue) for this turn,
    /// stripped from `content` by `ThinkBlockParser` on the agent
    /// side. Persisted as `Message::reasoning_content`. The desktop
    /// client uses this to restore the collapsible "思考过程" block
    /// on session reload — without it the user sees the reply but
    /// not the thinking that produced it.
    /// Populated for `role == "assistant"` only; `None` for all
    /// other roles (user, system, tool, approval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Execution timeline (tool_start / tool_stdout / tool_complete
    /// status / file_change events) emitted by the agent during
    /// this assistant turn. Mirrors `Message::execution_timeline`,
    /// which the server persists to `messages.execution_timeline`
    /// on flush.
    /// The desktop client feeds these straight into
    /// `expandTimelineToMessages` / `groupTimeline` to redraw the
    /// inline tool cards the user saw mid-stream — same status
    /// (running/done/error), same arguments, same stdout. Without
    /// it the reloaded session renders every tool call as a stale
    /// "运行中" pill.
    /// added — previous reloads had no way to
    /// reconstruct tool state because the WS stream wasn't
    /// persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_timeline: Option<Vec<ExecutionTimelineEvent>>,
    /// Frontend-built chronological reasoning/tools/text timeline for
    /// this assistant turn, persisted verbatim by the server and
    /// echoed back on reload so the desktop client restores the exact
    /// inline interleaving (tools shown where they happened) instead
    /// of the fixed `buildFallbackSteps` reasoning→tools→text order.
    /// Mirrors `Message::unified_steps`. `None` when the client didn't
    /// persist one (older turns) — the client then rebuilds via the
    /// fallback path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_steps: Option<Vec<serde_json::Value>>,
    /// wall-clock ISO-8601 timestamp read from
    /// `messages.created_at`. The desktop client uses this to
    /// render the per-message relative time ("3 天前") instead of
    /// falling back to `new Date()` in the wire adapter, which
    /// produced "刚刚 / 1 分钟前" on every reload. Populated only
    /// by `load_session_with_approvals_and_timestamps` (the HTTP
    /// `GET /v1/sessions/{id}/messages` handler); other writers
    /// (WS bridge, internal channels) leave it `None`, in which
    /// case the client falls back to `wire.timestamp ?? now`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// `Some(true)` marks this message as a copy returned from a
    /// compression child session (i.e. a turn that was folded
    /// away by the compression pipeline and is being shipped back
    /// to the client on session reload). The desktop client uses
    /// this to render a subtle "已压缩" marker on the message
    /// header so the user can tell which rows are history vs
    /// active. `None` for messages that were never archived.
    /// Added back to fix the `protocolMessage.compressed` field
    /// resolution error reported on (the field was
    /// referenced by `server::get_session_messages` but the
    /// transport struct definition was missing it, so the server
    /// crate no longer compiled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed: Option<bool>,
}

/// User's choice on a `RequestApproval` card.
/// Mirrors the upstream Hermes 4-state approval model
/// (`once / session / always / deny`):
/// - `Once`   — approve this single invocation. Server resolves to
/// `approved`; nothing is persisted.
/// - `Session` — approve for the rest of this session (or until the
/// agent process restarts, whichever comes first). Server resolves
/// to `approved` AND records the command in a per-session in-memory
/// set so subsequent same-shape tool calls bypass the prompt.
/// - `Always` — approve for all future sessions. Server resolves to
/// `approved` AND appends the command to the persistent
/// `$HERMIX_HOME/approval_allowlist.json` so it's loaded next
/// startup.
/// - `Deny`   — refuse this invocation. Server resolves to `denied`;
/// nothing is persisted. (The user can still click "always deny"
/// via the explicit `Deny` button in some clients, but the WS
/// protocol's `Deny` is single-shot — a future v3 protocol can add
/// `deny_session` / `deny_always` if needed.)
/// Free-form `String` on the wire so future variants (e.g.
/// `smart_skip` after an LLM risk-evaluation pass) don't require a
/// protocol bump. Server-side, unknown values are logged and
/// downgraded to `Deny` (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalChoice {
    Once,
    Session,
    Always,
    Deny,
}

impl ApprovalChoice {
    /// Parse from a free-form string (`"once"` / `"session"` /
    /// `"always"` / `"deny"`, case-insensitive). Unknown values
    /// return `None` so the caller can log + downgrade to deny.
    pub fn from_decision_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "once" => Some(Self::Once),
            "session" => Some(Self::Session),
            "always" => Some(Self::Always),
            "deny" | "denied" | "no" | "false" => Some(Self::Deny),
            // Legacy 2-state + common boolean variants: all downgrade
            // to single-shot Once (fail-open for known-good intents,
            // fail-closed only for explicit Deny variants).
            "approved" | "yes" | "true" => Some(Self::Once),
            _ => None,
        }
    }

    /// True if the choice grants the current tool call (so the agent
    /// can proceed). `Once`, `Session`, `Always` all do; `Deny`
    /// does not.
    pub fn is_approved(&self) -> bool {
        !matches!(self, Self::Deny)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub message_count: usize,
    /// The model used for this session (from sessions.model column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The workspace path selected for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The expert ID selected for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expert_id: Option<String>,
    /// Id of the workbench this session belongs to. `None` for an ordinary
    /// chat — those are the only ones the main sidebar lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workbench_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummary>,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessagesResponse {
    pub session_id: SessionId,
    pub messages: Vec<ProtocolMessage>,
    /// last persisted context-usage snapshot for the session,
    /// so the frontend can show the context-usage bar immediately after
    /// restoring an old session. `None` on sessions that never streamed
    /// (or pre-migration DBs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<hermix_core::types::ContextUsagePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSessionRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenameSessionRequest {
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendMessageRequest {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Override the system prompt for this turn (used when an "expert" persona
    /// injects custom instructions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Whitelist of tool names the agent may call this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Canonical working directory for this turn. Tools that touch the
    /// filesystem (terminal, file) constrain operations to this root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// `default` = ask before risk-bearing tool calls; `full` = auto-approve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// Optional images for vision-enabled models (多图支持).
    /// When present, the agent passes each image to the provider so the LLM
    /// can "see" them. Binary `Vec<u8>` straight from the client — no base64
    /// round-trip across the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageAttachment>>,
    /// Legacy single-image fields (kept for backward compat with older
    /// clients). Use `images` for new code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_mime_type: Option<String>,
    /// Names of skills to enable for this turn (ephemeral override of
    /// the server-side skill_store enabled-set). Backend feeds these into
    /// `skills_system_prompt` on top of any server-enabled skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Id of the workbench this session belongs to. `Some` gates the
    /// `workbench.context` direction and pins the `workbench` skill;
    /// ordinary chats are `None` and never see either (anti-leak).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workbench_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendMessageResponse {
    pub session_id: SessionId,
    pub reply: String,
    pub message_count: usize,
    pub trace_id: String,
}

/// One image attached to a chat turn .
/// `bytes` is the raw image file (PNG/JPEG/WebP), already compressed by
/// the client. Server-side decoding is a no-op since `Vec<u8>` is
/// directly usable as the data URL body for vision APIs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WsEnvelope {
    pub version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub trace_id: String,
    pub event: StreamEvent,
}

/// Default `outcome` for a `Done` event when the sender doesn't set
/// one (backward-compatible: older servers always meant "completed").
fn default_done_outcome() -> Option<String> {
    Some("completed".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Connected { session_id: SessionId },
    Text { content: String },
    Thinking { content: String },
    ToolStart {
        /// Stable id for this tool invocation. Server may emit
        /// several `ToolChunk` events carrying the same id before a
        /// matching `ToolComplete`. Frontend uses it to route chunks
        /// to the right `tool_calls[id].output` buffer.
        tool_call_id: String,
        tool: String,
        content: String,
    },
    ToolComplete {
        tool_call_id: String,
        tool: String,
        content: String,
        /// Set when the tool call was *denied* (user拒绝 / 超时 /
        /// 连接中断) rather than executed. `"policy"` means a
        /// hardline/Tirith gate blocked it (the LLM already knows
        /// from the `reason`); `"user_denied"` / `"timed_out"` /
        /// `"channel_closed"` are user-facing denials the frontend
        /// should surface as a "this call was blocked" banner so the
        /// agent's "tell the user + ask how to proceed" note is
        /// matched by a visible cue. `None` for a normal success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        denial_kind: Option<String>,
        /// Authoritative failure flag straight from `ToolResult::is_error`.
        /// `Some(true)` when the tool failed on execution or was denied;
        /// `Some(false)` on success; `None` for events that predate the
        /// flag. Lets the front-end mark failed calls from a fact instead
        /// of regex-guessing the payload text (which mis-flagged any normal
        /// output starting with "Error"/"Failed").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// A live chunk of tool output (stdout / stderr). Replaces the
    /// old ad-hoc `[tool: x] y (n/m)` text wrap. Carries enough
    /// metadata for the front-end to reassemble chunks in order and
    /// detect partial vs. final flushes.
    ToolChunk {
        tool_call_id: String,
        tool: String,
        content: String,
        chunk_index: u64,
        chunk_total: u64,
    },
    /// Server → client: a mutating tool (write_file / patch) just
    /// completed successfully. The chat UI uses this to render the
    /// WorkBuddy-style "文件变更 (N个)" summary row inline in the
    /// conversation. Carries the absolute file path, the change
    /// classification, and an optional one-line summary extracted
    /// from the tool output. See `agent_loop.rs::extract_file_change`.
    FileChange {
        tool_call_id: String,
        file_path: String,
        change_type: String, // "created" | "modified" | "deleted" | "renamed"
        file_summary: Option<String>,
    },
    Status { content: String },
    Activity { content: String },
    /// Context window usage snapshot. Emitted after every agent-loop
    /// turn so the frontend can render a persistent context-usage
    /// bar (e.g. "48k / 200k tokens (24%)").
    ContextUsage {
        /// Tokens currently consumed in the context window.
        used_tokens: u64,
        /// Maximum context window size for the active model.
        max_tokens: u64,
        /// Threshold at which auto-compression triggers.
        threshold_tokens: u64,
        /// Percentage of `max_tokens` currently consumed (0.0–100.0).
        percent: f64,
    },
    /// Compaction pipeline status snapshot. Emitted when the
    /// compression pipeline transitions state (pressure detected,
    /// breaker tripped, recovery probe, etc.).
    CompactionStatus {
        /// The `BreakerSnapshot` as a raw JSON value so the
        /// transport layer doesn't need to depend on the
        /// compression types.
        snapshot: serde_json::Value,
    },
    /// Per-turn token usage snapshot for the live
    /// "cache hit" chip in the chat header. Emitted after every
    /// successful LLM response so the front-end can update its
    /// session-level rolling cache hit rate in real time. Distinct
    /// from `ContextUsage` (which tracks window pressure) — this
    /// tracks what the provider actually billed.
    UsageUpdate {
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        reasoning_tokens: u64,
    },
    /// Server → client: "the agent is about to run a risk-bearing tool;
    /// tell me whether to proceed." The client echoes back via
    /// [`WsClientMessage::ApprovalResponse`] with the same `request_id`.
    RequestApproval {
        request_id: String,
        tool: String,
        arguments: serde_json::Value,
        /// "safe" | "caution" | "dangerous" (matches [`RiskLevel::as_str`]).
        risk: String,
        reason: Option<String>,
        /// Wall-clock deadline (Unix milliseconds) when the server
        /// will auto-deny this request. Drives the front-end
        /// countdown UI so the user always knows the budget they
        /// have left. The server always sends this (5 minutes by
        /// default — see `hermix_server::DEFAULT_APPROVAL_TIMEOUT`),
        /// but the field is `Option` for back-compat with older
        /// servers that pre-date the countdown feature
        /// . New clients should fall back to a sensible
        /// default (60s upstream-mirrored) if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        /// Available choices for the user. The 4-state model
        /// (`once / session / always / deny`) is the upstream
        /// canonical set. The server picks which subset to send:
        /// `deny` is always included; `session` / `always` are
        /// omitted if they wouldn't make sense (e.g. `always` for
        /// a one-off `read_file` call). The client should render a
        /// button per entry; clicking emits a
        /// [`WsClientMessage::ApprovalResponse`] whose `decision`
        /// matches the choice's `value`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choices: Vec<ApprovalChoice>,
        /// Unified before/after diff for file-mutating calls (backend-computed
        /// at request time — it needs the target file's current contents).
        /// `None` for non-file tools or shapes the backend can't diff; the
        /// client keeps its plain-description fallback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    /// Server → client: a **batch** of pending approval requests that
    /// the agent loop emitted in the same synchronous pre-dispatch
    /// pass (e.g. an LLM turn that fan-out-fanouts N parallel high-risk
    /// tool calls). The WS layer coalesces these into one envelope so
    /// the client can render one card and resolve all requests with a
    /// single button click. The `batch_id` is a runtime-only id —
    /// it is **not** persisted to SQLite; reloading the session will
    /// surface each `request_id` as its own `RequestApproval` row
    /// again, which is the intended semantics (history is per-call;
    /// the batch is a UI convenience for the live turn only).
    /// The client echoes back via
    /// [`WsClientMessage::ApprovalBatchResponse`] with the same
    /// `batch_id`. The single `decision` applies to every item in
    /// the batch — there is no per-item granularity at the wire
    /// level.
    RequestApprovalsBatch {
        batch_id: String,
        items: Vec<RequestApprovalItem>,
    },
    /// Surface memory writes / updates / deletes /
    /// summaries in the conversation so the user can always see what
    /// the agent is remembering. Bridged from the agent-loop
    /// `status_callback` event type `"memory_update"` (mirrors how
    /// `UsageUpdate` is bridged). One event per operation, or one
    /// batched event per turn when several facts are extracted at
    /// once (`count` carries the batch size).
    /// `action` is one of: `"write"` | `"update"` | `"delete"` |
    /// `"summarize"` | `"cleanup"` | `"snapshot"`.
    /// `memory_type` is one of: `"fact"` | `"style"` | `"summary"` |
    /// `"tool"` | `"snapshot"`.
    MemoryUpdate {
        action: String,
        memory_type: String,
        /// Canonical key — a fact's `canonical_key`, a `memory` tool
        /// `target`, or `"session"` for session-level summaries.
        key: String,
        /// One-line human-readable description of what changed.
        summary: String,
        /// Batch size when several operations are folded into one
        /// event (e.g. facts extracted from a single user turn).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<u64>,
    },
    /// Server → client: the user resolved a pending approval request.
    /// Emitted right before tool dispatch so the UI can dismiss the dialog
    /// deterministically.
    ApprovalResolved {
        request_id: String,
        /// The [`ApprovalChoice`] the user picked (serialized as
        /// `"once" | "session" | "always" | "deny"`). Older clients
        /// that only know the 2-state `approved` / `denied` will
        /// see `"once"` for an approved run and `"deny"` for a
        /// refused run, which preserves their existing semantics.
        decision: String,
    },
    /// (A): server → client: the agent recalled N related
    /// facts from memory this turn. Surfaced as a small "💭 想到 N 条"
    /// chip in the conversation. Different from `MemoryUpdate`
    /// (which fires on writes): this fires on reads.
    /// The full recall content is injected into the system prompt
    /// by `prompt_assembly`; this event only carries metadata
    /// (count + a small preview of `canonical_key`s) for the UI.
    MemoryRecall {
        /// Why the recall fired: `"first_turn" | "interval" | "drift"`.
        trigger: String,
        /// How many facts were pulled (≤ 5; see
        /// `recall_scheduler::MAX_RECALLS_PER_TURN`).
        count: u32,
        /// Total characters of the pulled payloads (≤ 4000).
        /// Optional so we can skip when 0 (most recalls hit at least
        /// one item, so this is rare in practice).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_chars: Option<u64>,
        /// Up to 3 `canonical_key`s for the UI preview.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preview_keys: Vec<String>,
        /// Which turn number this recall fired on.
        turn_number: u32,
    },
    /// Server → client: the agent called the `clarify` tool and is now
    /// blocked waiting for the user to answer. The client renders a
    /// question card and replies via [`WsClientMessage::ClarifyResponse`]
    /// with the same `request_id`.
    ClarifyRequest {
        request_id: String,
        question: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choices: Vec<String>,
    },
    /// Server → client: the user answered a pending clarify request.
    /// Emitted right after the user responds so the UI can clear the
    /// pending card deterministically.
    ClarifyResolved { request_id: String },
    /// H3: The agent's prefetcher identified a user intent and ran
    /// safe tools speculatively. Sent before LLM reasoning starts so
    /// the UI can display a "prefetch intent" chip.
    PrefetchIntent {
        /// Prefetch intent name (e.g. "weather_query", "daily_report").
        intent: String,
        /// Confidence in [0, 1]. Below 0.4 the agent loop skips prefetch.
        confidence: f32,
        /// Tools that were dispatched speculatively and their output.
        results: Vec<PrefetchResultEntry>,
    },
    /// ACUI (Agent-Controlled UI) directive.
    /// Agent 主动推送结构化 UI 卡片给前端。前端按 directive.id 维护
    /// 一组"已 show 未 hide"的卡片，按 `directive.kind` 查注册表渲染。
    /// 协议层独立于渲染实现（可换 React / Vue / Web Components）。
    Acui {
        /// The directive itself (`AcuiShow` / `AcuiUpdate` / `AcuiHide`).
        /// Tag is `"op"`; on the wire the inner variant is flattened.
        directive: hermix_core::AcuiDirective,
    },
    /// sub-agent's task goal, current phase (spawned → started → done /
    /// failed), turn count and estimated cost. The orchestrator emits one
    /// `sub_agent_update` chunk per phase transition; the server routes it
    /// here. `sub_agent_id` matches the id returned to the parent LLM so the
    /// user can correlate the panel with tool output.
    SubAgentEvent {
        /// Stable sub-agent id (e.g. `subagent-<uuid>`).
        sub_agent_id: String,
        /// Lifecycle phase: `spawned` | `started` | `done` | `failed`.
        phase: String,
        /// The task goal the parent delegated.
        task: String,
        /// Sub-agent role (`general` | `explore` | `plan`). `None` for legacy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// Current turn count (best-effort; `None` until the child reports).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turns: Option<u32>,
        /// Estimated cost in USD (if the child reported usage).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// Human-readable terminal error (only present on `failed` phase).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Error {
        code: String,
        message: String,
        /// Raw, un-localized backend error (e.g. an LLM 400 "invalid
        /// function arguments…" or a 5xx body). Surfaced to the frontend
        /// as the muted/collapsible "技术详情" subtext so the user sees
        /// the real cause instead of a generic banner. `None` when the
        /// error is already fully described by `message`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Done {
        content: String,
        /// Why the stream ended. Mirrors `AgentResult.finish_reason`.
        /// `None` when the reason is unknown (e.g. non-streaming path).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        /// Terminal outcome classification. `completed` = the turn
        /// finished naturally and the session is alive (front-end may
        /// close the WS). `failed` / `interrupted` = the agent loop
        /// ended without a usable result (backend error, invalid tool
        /// exhaustion, user stop). The front-end MUST keep the session
        /// alive on non-`completed` outcomes and surface a recoverable
        /// prompt instead of tearing down the WS — a backend failure is
        /// not a session death.
        #[serde(default = "default_done_outcome", skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        /// Raw backend error text on failed turns. Mirrors
        /// `AgentResult.last_error` — populated when `finish_reason`
        /// is `"error"` so the front-end can surface the real cause
        /// (e.g. an LLM 401 "invalid api_key" or a 5xx body) instead
        /// of a generic "后端错误" message. `None` for clean completions
        /// and user interrupts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    // ── Todo (task list) events ─────────────────────────────
    /// Snapshot of the full task list. Sent on (a) session start so
    /// the front-end can hydrate without a separate HTTP fetch, and
    /// (b) after every LLM-driven todo tool call so any late joiner
    /// catches up. Front-end uses this as the authoritative full
    /// state — applies upserts/removes lazily via TodoItem*.
    TodoSnapshot {
        session_id: String,
        items: Vec<TodoSnapshotItem>,
    },
    /// Single-item upsert. Emitted on create / update / depend /
    /// record_execution. Front-end merges by `id`. For "delete", see
    /// `TodoItemRemoved`.
    TodoItemUpserted {
        session_id: String,
        item: TodoSnapshotItem,
    },
    /// Single-item removal. Emitted when the LLM (or a future HTTP
    /// endpoint) deletes a task. Front-end filters by `id`.
    TodoItemRemoved {
        session_id: String,
        id: String,
    },
    /// LLM started a new plan. Emitted on the FIRST `todo.create` for a
    /// fresh group_id. Front-end creates a new inline card anchored to
    /// the assistant message with `message_id`. Subsequent TodoItemUpserted
    /// for the same group_id updates the existing card in place.
    /// `message_id` lets the front-end know where in the message stream
    /// to render the new card. `None` means the change came from outside
    /// an agent loop (HTTP PATCH / direct WS) and the front-end should
    /// fall back to anchoring at the end of the stream.
    TodoGroupStarted {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        group_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group_title: Option<String>,
    },
    //── Orchestration Engine events ───────────
    /// A new Task was created. Front-end can open a task card or
    /// refresh the task list panel.
    TaskCreated {
        task_id: String,
        title: String,
        mode: String,
        workspace_path: String,
    },
    /// A Task's fields were updated (title, context, result_refs, etc.).
    /// Front-end upserts the task card.
    TaskUpdated {
        task_id: String,
        title: String,
        status: String,
    },
    /// A Task's status transitioned (e.g. pending → in_progress →
    /// completed). Front-end updates the status badge and may trigger
    /// UI flows (e.g. show "方案确认" dialog on →planning, show
    /// results panel on →completed).
    TaskStatusChanged {
        task_id: String,
        old_status: String,
        new_status: String,
    },
    /// An automation fired and spawned a new Task. Front-end can
    /// show a toast notification and navigate to the new task.
    AutomationTriggered {
        automation_id: String,
        automation_name: String,
        task_id: String,
    },
    /// The agent produced deliverable artifacts for a Task. The
    /// front-end renders these in the "产物" tab of the results
    /// panel. `result_refs` carries the full snapshot so the client
    /// doesn't need a separate HTTP fetch.
    ResultDelivered {
        task_id: String,
        result_refs: Vec<hermix_core::TaskResultRef>,
    },
    /// Agent loop entered a new phase . Front-end can display
    /// a progress indicator showing which stage the agent is in
    /// (perception / reasoning / action / observation).
    LoopPhaseChanged {
        /// Session ID for correlation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Task ID if running within a Task context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// Fine-grained phase name (e.g. "llm_call", "tool_execution").
        phase: String,
        /// Coarse-grained orchestration stage
        /// (perception / reasoning / action / observation).
        stage: String,
        /// Turn number (1-based, 0 for Init).
        turn: u32,
    },
    /// Plan mode: the agent has produced a plan and is waiting for
    /// user confirmation before executing . Front-end should
    /// display the plan confirmation dialog.
    PlanConfirmationRequired {
        task_id: String,
        /// The plan text produced by the agent.
        plan_text: String,
    },
    /// Application-level WebSocket heartbeat. Emitted by the
    /// server every ~15s during a streaming session so the
    /// client can reset its idle/stale timer (the WS protocol
    /// ping alone doesn't reach the JS `onmessage` handler, so
    /// without this event the client would auto-settle the
    /// stream during long tool-only runs like `code_execution`
    /// that produce no `text` chunks for ≥90s). The client
    /// side (`useStreamChat.resetStaleTimer`) treats every WS
    /// event as a liveness ping, so the message body is empty
    /// after serde — only the envelope is required.
    /// Added back to fix the `StreamEvent::Heartbeat` variant
    /// resolution error reported on (the variant
    /// was referenced by `server::handle_ws_stream_send` but
    /// the transport enum definition was missing it).
    Heartbeat,
/// ( / ): server→client pong response to
/// a client-initiated application-level ping. The client sends
/// `WsClientMessage::Ping` every 10s; if it doesn't receive a
/// `Pong` for 2 consecutive cycles (20s), it closes the WS and
/// reconnects. This catches half-open connections where TCP stays
/// up but packets are silently dropped.
Pong,
}

/// One row in the task list. Same shape as
/// One item inside a [`StreamEvent::RequestApprovalsBatch`]. Same
/// per-call shape as a standalone `RequestApproval`, just bundled
/// so the client can render N sub-rows under one batch card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestApprovalItem {
    pub request_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub risk: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Backend-computed unified before/after diff for file-mutating calls.
    /// `None` when the shape can't be diffed — client keeps its fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// `hermix_tools::backends::todo::TodoItem`, but defined here so the
/// transport crate doesn't depend on hermix-tools (which would form
/// a cycle hermix-server → hermix-tools → hermix-transport).
/// Fields mirror `TodoItem` 1:1 — when adding a field there, mirror
/// it here too and bump the todo event protocol version in
/// `AGENTS.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoSnapshotItem {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// "pending" | "in_progress" | "completed" | "failed"
    pub status: String,
    /// "low" | "medium" | "high"
    pub priority: String,
    pub created_at: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    #[serde(default)]
    pub execution_count: u32,
    /// Plan / group id (UUID per "plan"). Empty string for legacy items.
    #[serde(default)]
    pub group_id: String,
    /// Human label for the plan (e.g. "rename all files"). Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_title: Option<String>,
    /// Session id this todo belongs to. Used by the front-end todo
    /// panel + post-snapshot merge logic to drop items that don't
    /// belong to the active tab. `None` for legacy items written
    /// before the per-session split. Commit A / C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Wire-format assistant message id that emitted the tool call
    /// which created this todo (`${session_id}-${rowIdx}`). Anchors
    /// the inline plan card to the originating assistant message
    /// after reload — same formula reproduces the same id from the
    /// persisted row order. `None` for legacy items. / C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_message_id: Option<String>,
}

/// Inbound message from the client over the streaming WebSocket. The server
/// handler must distinguish between a fresh user prompt ([`Self::Send`])
/// and an out-of-band response to a previously-issued approval request
/// ([`Self::ApprovalResponse`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    /// A new user message to dispatch (matches [`SendMessageRequest`]).
    Send {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        personality: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission: Option<String>,
        #[serde(default)]
        user_id: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
        /// Optional images (多图支持). Binary直传 — 客户端不做 base64。
        /// Server 接收后由 agent_loop 传给 provider。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageAttachment>>,
        /// Legacy single-image fields (kept for backward compat).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_mime_type: Option<String>,
        /// Names of skills to enable for this turn (ephemeral override of
        /// the server-side skill_store enabled-set). Server feeds these
        /// into `skills_system_prompt` on top of any server-enabled skills.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<String>>,
    },
    /// Response to a [`StreamEvent::RequestApproval`].
    /// The `decision` field is a free-form string that maps to
    /// [`ApprovalChoice`]: `"once"`, `"session"`, `"always"`, or
    /// `"deny"` (case-insensitive). The legacy 2-state values
    /// `"approved"` and `"denied"` are also accepted for backward
    /// compatibility with older desktop clients — `"approved"` maps
    /// to `ApprovalChoice::Once` (single-shot approve) and
    /// `"denied"` maps to `Deny`. Anything else is downgraded to
    /// `Deny` on the server side (fail-closed).
    ApprovalResponse {
        request_id: String,
        decision: String,
    },
    /// Response to a [`StreamEvent::RequestApprovalsBatch`]. One
    /// `decision` applies to **every** item in the batch — the
    /// protocol deliberately does not offer per-item granularity so
    /// the UI is a single card with N listed sub-actions and three
    /// buttons (deny / approve / always-approve).
    ///
    /// `decision` is the same free-form string as [`ApprovalResponse`]:
    /// `"approved"`, `"denied"`, `"always"`, `"session"`, `"once"`,
    /// `"deny"`, `"yes"`, `"no"`, `"true"`, `"false"` — all routed
    /// through [`ApprovalChoice::from_decision_str`] (case-insensitive,
    /// unknown values fail-closed to Deny).
    ///
    /// Wire tag is `approval_responses_batch` — the client mirrors the
    /// outbound `request_approvals_batch` envelope (approvals/batch +
    /// request→response symmetry), NOT the snake_case of the variant
    /// name (`approval_batch_response`). The `alias` keeps the
    /// snake_case form accepted so old frontends/clients still resolve.
    #[serde(rename = "approval_responses_batch", alias = "approval_batch_response")]
    ApprovalBatchResponse {
        batch_id: String,
        decision: String,
    },
    /// Response to a [`StreamEvent::ClarifyRequest`]. The free-form `answer`
    /// is what the user typed (or the choice they picked) and is fed back
    /// to the agent loop as the tool result.
    ClarifyResponse { request_id: String, answer: String },
    Ping,
}

pub fn ws_schema_json() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "HermesWsEnvelope",
        "type": "object",
        "required": ["version", "trace_id", "event"],
        "properties": {
            "version": { "type": "integer", "const": 1 },
            "request_id": { "type": "string" },
            "trace_id": { "type": "string" },
            "event": {
                "oneOf": [
                    { "type": "object", "required": ["type", "session_id"], "properties": { "type": { "const": "connected" }, "session_id": { "type": "string" } } },
                    { "type": "object", "required": ["type", "content"], "properties": { "type": { "const": "text" }, "content": { "type": "string" } } },
                    { "type": "object", "required": ["type", "content"], "properties": { "type": { "const": "thinking" }, "content": { "type": "string" } } },
                    { "type": "object", "required": ["type", "tool", "content"], "properties": { "type": { "const": "tool_start" }, "tool": { "type": "string" }, "content": { "type": "string" } } },
                    { "type": "object", "required": ["type", "tool", "content"], "properties": { "type": { "const": "tool_complete" }, "tool": { "type": "string" }, "content": { "type": "string" }, "denial_kind": { "type": ["string", "null"] } } },
                    { "type": "object", "required": ["type", "content"], "properties": { "type": { "const": "status" }, "content": { "type": "string" } } },
                    { "type": "object", "required": ["type", "content"], "properties": { "type": { "const": "activity" }, "content": { "type": "string" } } },
                    { "type": "object", "required": ["type", "request_id", "tool", "arguments", "risk"], "properties": { "type": { "const": "request_approval" }, "request_id": { "type": "string" }, "tool": { "type": "string" }, "arguments": { "type": "object" }, "risk": { "type": "string" }, "reason": { "type": ["string", "null"] }, "choices": { "type": "array", "items": { "type": "string", "enum": ["once", "session", "always", "deny"] } } } },
                    { "type": "object", "required": ["type", "request_id", "decision"], "properties": { "type": { "const": "approval_resolved" }, "request_id": { "type": "string" }, "decision": { "type": "string" } } },
                    { "type": "object", "required": ["type", "code", "message"], "properties": { "type": { "const": "error" }, "code": { "type": "string" }, "message": { "type": "string" } } },
                    { "type": "object", "required": ["type", "content"], "properties": { "type": { "const": "done" }, "content": { "type": "string" } } }
                ]
            }
        }
    })
}

pub fn new_trace_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_serialization_uses_tagged_union() {
        let event = StreamEvent::Text {
            content: "hello".into(),
        };
        let value = serde_json::to_value(event).expect("serialize stream event");
        assert_eq!(value["type"], "text");
        assert_eq!(value["content"], "hello");
    }

    #[test]
    fn envelope_contains_trace_and_version() {
        let envelope = WsEnvelope {
            version: 1,
            request_id: Some("req-1".into()),
            trace_id: "trace-1".into(),
            event: StreamEvent::Done {
                content: "ok".into(),
                finish_reason: None,
                outcome: Some("completed".into()),
                error: None,
            },
        };
        let value = serde_json::to_value(envelope).expect("serialize envelope");
        assert_eq!(value["version"], 1);
        assert_eq!(value["request_id"], "req-1");
        assert_eq!(value["trace_id"], "trace-1");
        assert_eq!(value["event"]["type"], "done");
    }

    #[test]
    fn request_approval_event_round_trips() {
        let event = StreamEvent::RequestApproval {
            request_id: "req-42".into(),
            tool: "terminal".into(),
            arguments: serde_json::json!({"command": "rm -rf /tmp/foo"}),
            risk: "dangerous".into(),
            reason: Some("destructive command".into()),
            expires_at: Some(1_700_000_000_000),
            choices: vec![],
            diff: None,
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["type"], "request_approval");
        assert_eq!(value["request_id"], "req-42");
        assert_eq!(value["tool"], "terminal");
        assert_eq!(value["risk"], "dangerous");
        assert_eq!(value["reason"], "destructive command");
        // `expires_at` is always present when the server populates it
        // (the countdown UI depends on it). The unit is Unix
        // milliseconds — picked because the front-end renders it
        // against `Date.now()` which is also ms.
        assert_eq!(value["expires_at"], 1_700_000_000_000_i64);
        // Empty `choices` is `skip_serializing_if = "Vec::is_empty"`.
        assert!(value.get("choices").is_none());

        let parsed: StreamEvent = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, event);
    }

    /// `RequestApproval::choices` round-trips the 4-state approval
    /// model — once / session / always / deny — over the wire.
    #[test]
    fn request_approval_event_with_choices_round_trips() {
        let event = StreamEvent::RequestApproval {
            request_id: "req-42".into(),
            tool: "terminal".into(),
            arguments: serde_json::json!({"command": "sudo apt install foo"}),
            risk: "caution".into(),
            reason: Some("requires confirmation".into()),
            expires_at: Some(1_700_000_300_000),
            choices: vec![
                ApprovalChoice::Once,
                ApprovalChoice::Session,
                ApprovalChoice::Always,
                ApprovalChoice::Deny,
            ],
            diff: None,
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["choices"], serde_json::json!(["once", "session", "always", "deny"]));
        assert_eq!(value["expires_at"], 1_700_000_300_000_i64);

        let parsed: StreamEvent = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, event);
    }

    /// When the server omits `expires_at` (skip_serializing_if), the
    /// front-end still has to deserialize the event without crashing.
    /// The `#[serde(default)]` on the field keeps backwards compat
    /// with pre-servers (the field didn't exist before the
    /// countdown feature was added).
    #[test]
    fn request_approval_event_without_expires_at_round_trips() {
        let event = StreamEvent::RequestApproval {
            request_id: "req-42".into(),
            tool: "terminal".into(),
            arguments: serde_json::json!({"command": "ls"}),
            risk: "safe".into(),
            reason: None,
            expires_at: None,
            choices: vec![],
            diff: None,
        };
        let value = serde_json::to_value(&event).expect("serialize");
        // `None` is `skip_serializing_if` — the field is absent on the
        // wire, exactly mimicking a pre-countdown server response.
        assert!(value.get("expires_at").is_none());

        let parsed: StreamEvent = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, event);
        assert!(matches!(parsed, StreamEvent::RequestApproval { expires_at: None, .. }));
    }

    #[test]
    fn approval_resolved_event_round_trips() {
        let event = StreamEvent::ApprovalResolved {
            request_id: "req-42".into(),
            decision: "session".into(),
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["type"], "approval_resolved");
        assert_eq!(value["decision"], "session");

        let parsed: StreamEvent = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, event);
    }

    /// `ApprovalChoice::from_decision_str` accepts the canonical
    /// 4-state names, the legacy 2-state names (`approved` / `denied`),
    /// and common boolean variants (`yes` / `no`, `true` / `false`).
    /// Unknown / empty / mismatched inputs return `None` so the
    /// caller can fail-closed.
    #[test]
    fn approval_choice_parsing() {
        // Canonical 4-state.
        assert_eq!(
            ApprovalChoice::from_decision_str("once"),
            Some(ApprovalChoice::Once)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("session"),
            Some(ApprovalChoice::Session)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("always"),
            Some(ApprovalChoice::Always)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("deny"),
            Some(ApprovalChoice::Deny)
        );
        // Case-insensitive.
        assert_eq!(
            ApprovalChoice::from_decision_str("ALWAYS"),
            Some(ApprovalChoice::Always)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("  Deny  "),
            Some(ApprovalChoice::Deny)
        );
        // Legacy 2-state back-compat.
        assert_eq!(
            ApprovalChoice::from_decision_str("approved"),
            Some(ApprovalChoice::Once),
            "legacy `approved` must downgrade to single-shot Once"
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("denied"),
            Some(ApprovalChoice::Deny)
        );
        // Common boolean / natural-language forms.
        assert_eq!(
            ApprovalChoice::from_decision_str("yes"),
            Some(ApprovalChoice::Once)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("no"),
            Some(ApprovalChoice::Deny)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("true"),
            Some(ApprovalChoice::Once)
        );
        assert_eq!(
            ApprovalChoice::from_decision_str("false"),
            Some(ApprovalChoice::Deny)
        );
        // Unknown / empty → None (caller should fail-closed).
        assert_eq!(ApprovalChoice::from_decision_str(""), None);
        assert_eq!(ApprovalChoice::from_decision_str("maybe"), None);
        assert_eq!(ApprovalChoice::from_decision_str("alwaysdeny"), None);

        // `is_approved` is true for everything except Deny.
        assert!(ApprovalChoice::Once.is_approved());
        assert!(ApprovalChoice::Session.is_approved());
        assert!(ApprovalChoice::Always.is_approved());
        assert!(!ApprovalChoice::Deny.is_approved());
    }

    /// Wire-level: `WsClientMessage::ApprovalResponse` with the new
    /// 4-state `decision` values round-trips, and the legacy
    /// `approved` / `denied` values still parse (so an old desktop
    /// client can still drive a new server).
    #[test]
    fn approval_response_4state_wire_round_trip() {
        for decision in ["once", "session", "always", "deny", "approved", "denied", "ALWAYS"] {
            let msg = WsClientMessage::ApprovalResponse {
                request_id: "req-1".into(),
                decision: decision.into(),
            };
            let value = serde_json::to_value(&msg).expect("serialize");
            assert_eq!(value["type"], "approval_response");
            assert_eq!(value["decision"], decision);

            let parsed: WsClientMessage =
                serde_json::from_value(value).expect("deserialize");
            assert_eq!(parsed, msg, "round-trip failed for `{}`", decision);
        }
    }

    #[test]
    fn ws_client_message_approval_response_round_trips() {
        let msg = WsClientMessage::ApprovalResponse {
            request_id: "req-1".into(),
            decision: "denied".into(),
        };
        let value = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(value["type"], "approval_response");
        assert_eq!(value["request_id"], "req-1");
        assert_eq!(value["decision"], "denied");

        let parsed: WsClientMessage = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn ws_client_message_send_round_trips() {
        let msg = WsClientMessage::Send {
            text: "hi".into(),
            model: Some("gpt-4o".into()),
            provider: None,
            personality: None,
            system_prompt: None,
            tools: None,
            workspace: None,
            permission: Some("full".into()),
            user_id: None,
            request_id: Some("req-2".into()),
            images: None,
            image: None,
            image_mime_type: None,
            skills: Some(vec!["git".into(), "code-review".into()]),
        };
        let value = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(value["type"], "send");
        assert_eq!(value["text"], "hi");
        assert_eq!(value["permission"], "full");
        // None fields should be skipped
        assert!(value.get("provider").is_none());

        let parsed: WsClientMessage = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn fixtures_round_trip_across_protocol_matrix() {
        let fixtures = include_str!("../../../sdk/protocol-fixtures/ws_envelopes.json");
        let parsed: Vec<WsEnvelope> =
            serde_json::from_str(fixtures).expect("parse protocol fixtures");
        assert_eq!(parsed.len(), 4);
        assert!(matches!(parsed[0].event, StreamEvent::Connected { .. }));
        assert!(matches!(parsed[1].event, StreamEvent::Text { .. }));
        assert!(matches!(parsed[2].event, StreamEvent::Error { .. }));
        assert!(matches!(parsed[3].event, StreamEvent::Done { .. }));
    }
}
