/**
 * Lifecycle states for a pending tool-approval card.
 *
 * - `"approved"` — the user clicked Allow while the resolver
 *   was still live; the decision was forwarded to the agent
 *   and the tool ran.
 * - `"denied"` — the user clicked Deny while the resolver
 *   was still live; the agent was told to skip the call.
 * - `"expired"` — the user clicked *after* the resolver was
 *   already gone (e.g. the 5-minute
 *   `DEFAULT_APPROVAL_TIMEOUT` on the server had already
 *   auto-denied the call, or the WebSocket closed). The
 *   click did NOT reach the agent; the displayed state is
 *   honest about that and the user is told the action was
 *   not performed. This is the state the owner requested
 *   on after seeing the card flip to "已允许"
 *   while the LLM acted as if denied — a UI/backend
 *   desync caused by a silent stale-click path.
 */
export type ApprovalDecision = "approved" | "denied" | "expired";

/**
 * Metadata for an inline approval card sandwiched between assistant
 * messages. Mirrors the server-side `ApprovalRecord`
 * (`hermix-agent::session_persistence::ApprovalRecord`) one-to-one so we
 * can pass the JSON `extra` field through unchanged.
 *
 * The `decision` field is `undefined` while the request is still
 * pending. Once the user clicks allow/deny, we update the row in
 * place (and persist the decision server-side via the WebSocket
 * `approval_response` round-trip).
 */
export interface ApprovalMessageMeta {
  requestId: string;
  tool: string;
  args?: unknown;
  riskLevel?: string;
  decision?: ApprovalDecision | null;
  decidedAt?: string | null;
  /** Wall-clock deadline (Unix milliseconds) when the server
   *  will auto-deny this request. Mirrors the server's
   *  `StreamEvent::RequestApproval::expires_at` field on the
   *  wire and `ApprovalRecord::expires_at` in SQLite.
   *
   *  `undefined` / `null` for rows persisted before the
   *  countdown feature shipped (2026-06-14) — the front-end
   *  falls back to a 60s default in that case.
   *
   *  Why we persist this instead of re-deriving on reload: a
   *  session reload would otherwise show a freshly computed
   *  deadline that's a few seconds in the future, but the
   *  server-side timeout has *already* fired by then (the
   *  back-end auto-denied while the page was reloading). The
   *  persisted value lets the front-end render the honest
   *  "已超时" pill against the *original* deadline, not a
   *  confusing "4 minutes left" countdown that's actually
   *  invalid. */
  expiresAt?: number | null;
  /** Frontend-generated id of the assistant message this approval is
   *  visually attached to. The server doesn't enforce this — it's a
   *  hint for "the previous assistant turn ended just before this
   *  approval". For approvals persisted before this field existed
   *  (legacy rows), it's `undefined`. */
  linkedAssistantId?: string | null;
}

/**
 * Metadata for an inline clarify card. When the agent calls the
 * `clarify` tool the server emits a `StreamEvent::ClarifyRequest` which
 * the chat layer turns into a `role="clarify"` message. When the user
 * answers, the WebSocket layer sends a `clarify_response` back and we
 * fill in `answer` for the resolved footer.
 */
export interface ClarifyMessageMeta {
  requestId: string;
  question: string;
  choices: string[];
  answer?: string | null;
}

/**
 * Metadata for an inline tool-execution message (`role: "tool"`).
 *
 * These messages appear interleaved with user/assistant messages in
 * the conversation flow — each tool call gets its own inline card
 * that shows real-time stdout while running, then collapses to a
 * "done" pill when complete.
 *
 * During a live stream, the card is inserted at the point where the
 * tool was called (after the preceding assistant text, before the
 * following). On session reload, tool messages are reconstructed
 * from the server's `execution_timeline` array.
 */
export interface ToolMessageMeta {
  /** Tool name (e.g. "terminal", "read_file"). */
  tool: string;
  /** Raw arguments JSON string from the tool call. */
  args?: string;
  /** Live state — flows running → done (or error). */
  status: "running" | "done" | "error";
  /** Accumulated stdout/stderr. Streamed live in 4KB chunks. */
  output?: string;
  /** ISO timestamp when the tool invocation started. */
  startedAt: string;
  /** ISO timestamp when the tool completed (or errored). */
  completedAt?: string;
  /**
   * Stable call-id that matches the `tool_call_id` in the WS stream.
   * Used to route `tool_chunk` / `tool_complete` events to the correct
   * card when multiple tools run concurrently.
   */
  toolCallId?: string;
  /**
   * The assistant message whose content immediately precedes this
   * tool call. When the tool card is rendered, it anchors visually
   * to this message so the user can see "the LLM wrote X, then called
   * tool Y, then continued with Z".
   */
  linkedAssistantId?: string;
  /**
   * Short human-readable reason the tool ended in error. Populated
   * by `expandTimelineToMessages` for two cases:
   *   - the close event carried an error payload (exit_code ≠ 0,
   *     JSON `error` field, etc.)
   *   - the tool started but never produced a `tool_complete` event
   *     ("stranded" — most commonly a session reload while a tool
   *     was in-flight, where the live process is long gone)
   *
   * Renderers use this instead of inventing their own copy. Older
   * renders that pre-date this field simply ignore it.
   */
  errorReason?: string;
  /**
   * Memory-tool-only: the action performed (add / replace / remove /
   * read), populated from the tool's structured JSON result. Lets the
   * card show "编辑记忆 / 更新记忆 / 删除记忆 / 查找记忆" instead of a
   * generic verb.
   */
  action?: string;
  /**
   * Memory-tool-only: absolute path of the memory file the operation
   * touched (e.g. `…/MEMORY.md` / `…/USER.md`). Rendered as a
   * clickable link that opens a preview.
   */
  file_path?: string;
  /**
   * Memory-tool-only: structured outcome from the tool's JSON result
   * (`success` / `not_found` / `ambiguous` / `error`). Drives the
   * badge text — `not_found` renders "未查到对应内容", NOT "失败".
   */
  memoryStatus?: "success" | "not_found" | "ambiguous" | "error";
}

// ---------------------------------------------------------------------------
// ChatMessage — canonical in-memory chat-row type.
// Before the front-end had three near-identical types describing the
// same entity: a wire shape (snake_case `extra`, no UI state), `ChatMessage`
// (in-memory typed projection, UI-only fields), and ad-hoc inline literals
// at call sites. Every wire → UI projection had to be kept in sync manually.
// collapses them: `ChatMessage` is the single source of truth, wire
// fields live alongside UI fields. The wire → UI projection is centralised
// in `protocolAdapter.ts` (`wireToChatMessage`) so adding a new
// approval/clarify/tool field is a one-file change.
// ---------------------------------------------------------------------------
export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system" | "approval" | "clarify" | "tool";
  content: string;
  timestamp: string;
  model?: string;
  execution_backend?: "local" | "sandbox";
  tool_calls?: ToolCall[];
  execution_timeline?: ExecutionTimelineEvent[];
  /// Why the assistant stopped. Mirrors `finish_reason` from the model.
  /// `stop` = normal completion, `length` = truncated,
  /// `interrupted` = user cancelled, `tool_calls` = tools pending.
  stopReason?: string;
  /// Additional styling hint for the stop-reason chip. Overrides the
  /// default red/error tone for transient issues like stream errors
  /// that are not user-caused.
  stopReasonMeta?: "stream-error" | "non-error" | "tool-approval";
  /// Populated when `role === "approval"`. Server delivers this in
  /// the `extra` JSON field; the renderer maps it into the
  /// `InlineApprovalCard` props.
  approval?: ApprovalMessageMeta;
  /// Populated when `role === "clarify"`. Mirrors the server-side
  /// `PendingClarify` fields exposed via `StreamEvent::ClarifyRequest`.
  clarify?: ClarifyMessageMeta;
  /// Populated when `role === "tool"`. Drives the inline tool-execution
  /// card that sits between assistant messages in the conversation flow.
  tool?: ToolMessageMeta;
  /**
   * The model's raw reasoning (thinking monologue) for this turn,
   * stripped from the user-visible `content` by `ThinkBlockParser`.
   * Streamed live via the `thinking` WS event and persisted in
   * `Message::reasoning_content` server-side, so a session reload
   * re-displays the same thought process the user saw during the
   * live stream.
   *
   * Empty string / `undefined` = the model didn't emit a think block
   * (or emitted only `<think></think>`).
   */
  reasoningContent?: string;
  /**
   * If true, the inline reasoning block is expanded in the message
   * card. Persists in component state per message; clicking the
   * header toggles it. Defaults to `false` (collapsed) so the chat
   * scroll position isn't pushed by long thinking text.
   */
  reasoningExpanded?: boolean;
  /**
   * Wall-clock time the LLM spent on this turn, in
   * milliseconds. Populated by the front-end at the `Done` event
   * (useStreamChat measures `Date.now() - sendStartedAt`); NOT
   * persisted server-side, so it's only present for messages that
   * were streamed live in the current session. Reloaded history
   * gracefully falls back to "已完成" without a duration.
   *
   * The "ProcessTimeline" header reads this to render
   * "已完成 10s" / "已完成 1m 23s" instead of the bare "已完成".
   */
  durationMs?: number;
  /**
   * Unified timeline steps — chronological sequence of reasoning /
   * tools / text chunks. When present, the renderer uses this instead
   * of the fixed 3-section layout so multi-iteration agent loops
   * display each step as a cohesive unit. When absent, the renderer
   * falls back to `buildFallbackSteps()` which synthesises a simple
   * 3-step list from the legacy fields.
   */
  unified_steps?: TimelineStep[];
  /**
   * User-attached images for this message (display only). `blobUrl` is
   * an in-memory object URL valid for the live session — it is NOT
   * persisted across reloads (the backend stores the raw bytes; the
   * front-end would need to re-fetch and recreate the blob to show
   * images after a session reload).
   */
  images?: ChatMessageImage[];
/// Context-compression marker: when `true`, the bubble shows a "已压缩"
/// badge to indicate this message was compacted by the context engine.
compressed?: boolean;
  // ── wire-only carry-over fields ─────────────────────────────────
  // Carried verbatim from the JSON response so a session reload shows
  // the same content the user saw mid-stream. The typed-`tool` /
  // typed-`approval` props above are derived from these by
  // `protocolAdapter.wireToChatMessage`; we keep the raw form here
  // for any future field that hasn't been typed yet.
  tool_name?: string;
  tool_arguments?: string;
  extra?: {
    request_id?: string;
    tool?: string;
    arguments?: unknown;
    risk?: string;
    decision?: "approved" | "denied" | null;
    decided_at?: string | null;
    expires_at?: number | null;
    linked_assistant_id?: string | null;
  };
}

/**
 * A user-attached image surfaced inside the user message bubble.
 * `blobUrl` is an in-memory object URL valid only for the live session —
 * it is NOT persisted across reloads.
 */
export interface ChatMessageImage {
  blobUrl: string;
  mimeType: string;
  name: string;
}

/**
 * A user attachment buffered in the send queue while a previous turn is
 * still streaming. `file` carries the original handle so the drain path
 * can re-read the bytes; `blobUrl` renders the preview in the bubble.
 */
export interface QueuedImage {
  file: File;
  blobUrl: string;
  mimeType: string;
  name: string;
}

/** An item held in `messageQueue` between turns. */
export interface QueuedMessage {
  text: string;
  images?: QueuedImage[];
  attachedFilePaths?: string[];
}

export interface ToolCall {
  name: string;
  status: "running" | "done" | "error";
  output?: string;
}

export interface ExecutionTimelineEvent {
  type: "tool_start" | "tool_stdout" | "tool_complete" | "status" | "file_change" | "section_gate" | "reasoning" | "text" | "memory_update" | "memory_recall";
  tool?: string;
  content?: string;
  arguments?: string;
  chunk_index?: number;
  chunk_total?: number;
  created_at: string;
  /** Stable WS `tool_call_id` for tool_* events. Used by callers that
   *  need to correlate streamed chunks with their parent invocation
   *  (e.g. timeline parsers that want to group stdout by call). */
  tool_call_id?: string;
  /** file_change-only fields */
  file_path?: string;
  change_type?: "created" | "modified" | "deleted" | "renamed";
  file_summary?: string;
  /** memory_update-only fields — surfaced inline so the user can see
   *  what the agent is remembering (writes / updates / deletes /
   *  summaries). Mirrors `StreamEvent::MemoryUpdate` in hermix-transport. */
  memory_action?: "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
  memory_type?: "fact" | "style" | "summary" | "tool" | "snapshot" | "reflection";
  memory_key?: string;
  memory_summary?: string;
  memory_count?: number;
  /** memory_recall-only fields — surfaced inline as a small chip so the
   *  user can see "the agent thought of N related facts this turn".
   *  Mirrors `StreamEvent::MemoryRecall` in hermix-transport. */
  memory_recall_trigger?: "first_turn" | "interval" | "drift";
  memory_recall_count?: number;
  memory_recall_total_chars?: number;
  memory_recall_preview_keys?: string[];
  memory_recall_turn_number?: number;
  /** (C3 marker protocol): section_gate-only payload.
   *  Carries the serialised `SectionGateReport` from the backend.
   *  `patched` lists sections the gate auto-spliced in;
   *  `pending` lists optional sections absent under the current
   *  `RouteContext`. The trace panel renders one chip per entry
   *  using `Section::label()` from the backend, so the front-end
   *  never has to maintain a hard-coded label table. */
  section_gate_report?: SectionGateReport;
}

/**
 * Live sub-agent (`delegate_task`) lifecycle phase. Mirrors the
 * `phase` field of `StreamEvent::SubAgentEvent` in hermix-transport.
 * Transition order: `spawned` → `started` → `done` / `failed`.
 */
export type SubAgentPhase = "spawned" | "started" | "done" | "failed";

/**
 * Live sub-agent state, keyed by `subAgentId` in a session-level
 * `Record`. The "子代理" panel updates the matching row on each phase
 * transition so the user can watch delegated work in real time.
 */
export interface SubAgentState {
  subAgentId: string;
  phase: SubAgentPhase;
  task: string;
  role?: "general" | "explore" | "plan";
  turns?: number;
  costUsd?: number;
  error?: string;
  /** Front-end-side wall-clock of the last phase update. */
  updatedAt: number;
}

/** Payload delivered by `useStreamChat`'s `onSubAgentEvent` callback. */
export interface SubAgentEventPayload {
  subAgentId: string;
  phase: SubAgentPhase;
  task: string;
  role?: "general" | "explore" | "plan";
  turns?: number;
  costUsd?: number;
  error?: string;
}

/**
 * Unified timeline step — one "chunk" of the assistant's response in
 * chronological order. Replaces the fixed 3-section layout
 * (reasoning → tools → text) with an interleaved sequence so the user
 * can follow each iteration of the agent loop as a cohesive unit:
 *
 *   [reasoning 1] → [tools 1] → [reasoning 2] → [tools 2] → [text]
 *
 * Built from per-iteration assistant rows in
 * `mergeConsecutiveAssistantMessages` (reload path) or from real-time
 * phase tracking in streaming handlers (streaming path). When absent,
 * `buildFallbackSteps` in `messageParser.ts` synthesises a simple
 * 3-step list from the legacy `reasoningContent` / `execution_timeline`
 * / `content` fields.
 */
export type TimelineStep =
  | { kind: "reasoning"; content: string }
  | { kind: "tools"; events: ExecutionTimelineEvent[] }
  | { kind: "text"; content: string };

/** (C3 marker protocol): wire-mirror of the backend's
 *  `SectionGateReport`. Field names match the `serde(rename=...)`
 *  tags emitted by `crates/hermix-agent/src/context.rs`. */
export interface SectionGateReport {
  patched: SectionGapWire[];
  pending: SectionGapWire[];
}

export interface SectionGapWire {
  section: "identity" | "tool_list" | "tool_enforce" | "working_dir" | "timestamp";
  /** Backend `SectionGap::patched` is renamed to `applied` on the
   *  wire (see `SectionGap`'s `#[serde(rename = "applied")]`). */
  applied: boolean;
  reason: string;
}

export interface Session {
  id: string;
  title: string;
  project?: string;
  messages: ChatMessage[];
  /**
   * Backend-reported message count for the session. Loaded eagerly
   * from `GET /v1/sessions`; unlike `messages`, which only loads when
   * the user opens the session. Used by the sidebar to render the
   * "N 条" badge and by `handleNewChat` to decide whether an existing
   * empty session can be reused.
   *
   * `messages.length` is NOT a reliable indicator of emptiness:
   * `toSession()` always seeds it as `[]`, so `messages.length === 0`
   * is true for *every* session until the user has clicked into it.
   */
  message_count?: number;
  created_at: string;
  updated_at: string;
  /** Workspace path (from sessions.workspace column). */
  workspace?: string;
  /** Active model for this session (from sessions.model column). */
  model?: string;
  /** Selected expert ID for this session (from sessions.expert_id column). */
  expert_id?: string;
}

export interface Project {
  id: string;
  name: string;
  path: string;
}

export interface RunRecord {
  at: string;
  status: "ok" | "failed" | "running";
  duration_ms: number;
  summary?: string;
  error?: string;
}

export interface AutomationTask {
  id: string;
  name: string;
  description?: string;
  prompt: string;
  schedule: string;
  enabled: boolean;
  schedule_label?: string;
  model?: string;
  expert_id?: string;
  workspace_path?: string;
  permission_level?: "default" | "full";
  category?: string;
  icon?: string;
  last_run_at?: string;
  last_status?: "ok" | "failed" | "running";
  last_error?: string;
  next_run_at?: string;
  run_count: number;
  history: RunRecord[];
  created_at: string;
  updated_at: string;
  /** skills attached to this cron job — maps to CronJob.skills. */
  skills?: string[];
}

export interface AppConfig {
  api_base: string;
  default_model: string;
  theme: string;
  mode: "local" | "remote";
}

export interface PluginSettings {
  mcp_filesystem: boolean;
  mcp_terminal: boolean;
  mcp_browser: boolean;
  mcp_database: boolean;
  tool_code_exec: boolean;
}

export type NavPage = "chat" | "search" | "plugins" | "automation" | "settings" | "agents" | "groupchat" | "experts" | "analytics" | "customapps";
// ↑ "experts" is kept for backward compat — it's now an alias of "agents"
// with initialTab="builtin". Sidebar no longer exposes it directly.

// Re-export from api
export type { SessionSummary, SystemStatus, HermesConfig, SkillInfo } from "./api";
