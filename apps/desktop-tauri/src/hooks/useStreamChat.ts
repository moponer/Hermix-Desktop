import { useState, useCallback, useRef, useEffect } from "react";
import { resolveWebSocketUrl } from "../api";
import { diagLog, diagWarn, nextDiagId } from "../lib/diag";
import type { AcuiDirective } from "../acui/types";
import type { SubAgentEventPayload } from "../types";

/**
 * Single row in the live task list. Mirrors
 * `hermix_transport::TodoSnapshotItem` (which mirrors
 * `hermix_tools::backends::todo::TodoItem` — keep all three in
 * sync when adding fields).
 *
 * Field-level semantics:
 * - `id`: stable across the session, generated server-side (UUIDv4).
 * - `status`: "pending" | "in_progress" | "completed" | "failed".
 * - `priority`: "low" | "medium" | "high" (default "medium").
 * - `dependsOn`: array of task ids. Empty for tasks with no deps.
 *   Frontend renders these as small "blocked by X" chips.
 * - `executionCount`: how many times the agent tried this task.
 * - `lastResult`: free-form text from the most recent
 *   `record_execution` call. Useful for "why did this fail?" hover.
 */
export interface TodoSnapshotItem {
  id: string;
  title: string;
  description?: string | null;
  status: string;
  priority: string;
  created_at: string;
  depends_on: string[];
  last_result?: string | null;
  execution_count: number;
  /** Plan / group id (UUID per plan). Empty string for legacy items. */
  group_id: string;
  /** Human label for the plan (e.g. "rename all files"). Optional. */
  group_title?: string | null;
}

/**
 * Lifecycle phase of a single chat send. Returned by `useStreamChat`
 * so the caller can drive the UI through every stage without
 * collapsing them all into a single boolean "isStreaming" flag — that
 * flag let "已中断" and full reply text appear at the same time, so the
 * phase machine keeps each stage distinct.
 *
 * Transitions:
 *   idle → connecting → streaming → (slow | completed | error | timeout | partial)
 *
 * • `slow`     — 30s elapsed without a first chunk; the stream is
 *                still alive (WS open, no `done`/`error` yet), the
 *                UI should swap the spinner for a "响应较慢" hint
 *                but **must not** mark the message as interrupted.
 * • `partial`  — WS closed mid-stream after some content arrived
 *                (server crash, network blip). The partial text
 *                is preserved; the chip is "AI 响应未完成（已收到部分内容）"
 *                in muted tone, not red.
 * • `timeout`  — 300s elapsed with no data received (idle);
 *                the stream is genuinely abandoned and the chip
 *                is "AI 响应超时" in warning tone. The timer resets
 *                on every received WS event, so long-running tasks
 *                that are actively streaming will never hit this.
 * • `error`    — server sent an `error` event, or WS closed with
 *                no content. Chip is "出错了" in muted tone.
 * • `completed`— server sent `done` with `finish_reason: "stop"`.
 */
export type StreamPhase =
  | "idle"
  | "connecting"
  | "streaming"
  | "slow"
  | "reconnecting"
  | "completed"
  | "error"
  | "timeout"
  | "partial";

/**
 * Per-request overrides sent on the initial WS frame after `connected`.
 * Mirrors the backend's `SendMessageRequest` extension fields
 * (`hermix-transport/src/lib.rs`).
 */
export interface ChatOverrides {
  /** Optional path the agent's read/write tools are restricted to. */
  workspace?: string;
  /** Inline system-prompt block prepended at the top of the agent's prompt. */
  systemPromptOverride?: string;
  /** Pre-resolved tool whitelist. Populated by `ChatView.handleSubmit`
   *  from the live `GET /v1/tools` response — filtered by either
   *  the expert's `toolset` tag or the registry's `isEnabled` flag.
   *  Backend (`local_agent_service::apply_overrides` → `bridge_tool_registry`)
   *  applies this as the per-turn tool allow-list. */
  tools?: string[];
  /** Backend `personality` slug or inline blob. */
  personality?: string;
  /** Permission level — "default" (ask on risky) | "full" (no ask). */
  permissionLevel?: "default" | "full";
  /** 模型路由 — 形如 "minimax:MiniMax-M2.7" 的 "provider:model" 串。
   *  来自 ChatView 的 selectedModel（用户在模型下拉里切换的结果）。
   *  透传到 backend `SendMessageRequest.model` → `AgentOverrides.model` →
   *  `effective_model` → `resolve_provider()`，最终决定这次请求走哪个 provider。
   *  没设 = 用 backend config.yaml 的默认 model。
   *  见 `crates/hermix-agent/src/local_agent_service.rs::send_message:871-888` */
  model?: string;
  /** Names of skills to enable for this turn (ephemeral override of the
   *  server-side skill_store enabled-set). Backend feeds these into
   *  `skills_system_prompt` on top of any server-enabled skills. */
  skills?: string[];
  /** Free-form overrides for future use; backend ignores unknown keys. */
  /** 多图支持 — binary bytes 直传，不走 base64。 */
  images?: { bytes: number[]; mimeType: string; name?: string }[];
  image?: string;  // legacy single-image base64 (backward compat)
  imageMimeType?: string;  // mime type for legacy single image
  /** 非图片本地文件路径（拖放/粘贴时获取），后端直接用 read_file/read_pdf 读取 */
  attachedFilePaths?: string[];
  /** Extensibility index — backend ignores unknown keys. */
  [key: string]: unknown;
}

/** One prefetch result entry sent over the wire (mirrors
 *  `hermix-transport::PrefetchResultEntry`). */
export interface PrefetchResultEntry {
  tool: string;
  args: unknown;
  output: string;
  errored: boolean;
  durationMs: number;
}

interface StreamCallbacks {
  /** Called on every phase transition. Caller may use this to render a
   *  phase-aware status chip on the streaming assistant message. */
  onPhaseChange?: (phase: StreamPhase) => void;
  onChunk?: (text: string) => void;
  onToolStart?: (info: { toolCallId: string; tool: string; content?: string }) => void;
  onToolComplete?: (info: { toolCallId: string; tool: string; content?: string; denialKind?: string }) => void;
  /**
   * A live chunk of tool output (stdout/stderr). `chunkIndex` /
   * `chunkTotal` let the front-end detect partial flushes and reorder
   * out-of-order delivery. Server emits the chunks in order; the
   * `toolCallId` is stable across a single invocation and matches
   * the surrounding `onToolStart` / `onToolComplete` calls.
   */
  onToolChunk?: (info: {
    toolCallId: string;
    tool: string;
    content: string;
    chunkIndex: number;
    chunkTotal: number;
  }) => void;
  /**
   * A mutating tool (write_file / patch) just completed and the
   * server has reported the file change. The front-end uses this to
   * render the WorkBuddy-style "文件变更 (N个)" summary block inline
   * in the conversation. Mirrors `StreamEvent::FileChange` in
   * hermix-transport — payload fields are stable across the
   * WS → message-persistence → reload boundary.
   */
  onFileChange?: (info: {
    toolCallId: string;
    filePath: string;
    changeType: "created" | "modified" | "deleted" | "renamed";
    fileSummary?: string;
  }) => void;
  /**
   * A memory mutation (write / update / delete / summarize / cleanup)
   * was surfaced by the agent. Mirrors `StreamEvent::MemoryUpdate` in
   * hermix-transport. The front-end renders it as an inline
   * "记忆" row in the conversation so the user can see what the agent
   * is remembering. Payload fields are stable across the
   * WS → message-persistence → reload boundary.
   */
  onMemoryUpdate?: (info: {
    action: "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
    memoryType: "fact" | "style" | "summary" | "tool" | "snapshot" | "reflection";
    key: string;
    summary: string;
    count?: number;
  }) => void;
  /**
   * Recall event: fired by the server when the agent recalls N
   * related facts from memory this turn. The full content is
   * injected into the system prompt by `prompt_assembly`; this
   * callback only carries a small preview for the UI chip.
   */
  onMemoryRecall?: (info: {
    trigger: "first_turn" | "interval" | "drift";
    count: number;
    totalChars?: number;
    previewKeys: string[];
    turnNumber: number;
  }) => void;
  /**
   * A sub-agent (`delegate_task`) lifecycle phase transition. Mirrors
   * `StreamEvent::SubAgentEvent` in hermix-transport. The front-end
   * keeps a session-level map keyed by `subAgentId` and updates the
   * matching row on each event (spawned → started → done / failed).
   */
  onSubAgentEvent?: (info: SubAgentEventPayload) => void;
  /**
   * A todo list mutation fired by the server. Four kinds:
   *
   *  - `snapshot`: full authoritative state (sent on session start
   *    and after every LLM `todo` tool call by the server). Caller
   *    should rebuild `todoGroups` from this. Mirrors
   *    `StreamEvent::TodoSnapshot` in hermix-transport.
   *  - `group_started`: LLM declared a new plan (first `todo.create`
   *    for a fresh group_id). Front-end creates a new inline card
   *    anchored to the assistant message with `messageId` (or end of
   *    stream when the server omitted it).
   *  - `upsert`: create / update / depend / record_execution.
   *  - `remove`: LLM (or HTTP DELETE) removed the item.
   *
   * Mirrors `StreamEvent::TodoSnapshot` / `TodoGroupStarted` /
   * `TodoItemUpserted` / `TodoItemRemoved` in hermix-transport.
   * `sessionId` is stamped by the server based on which WS
   * connection produced the change.
   */
  onTodoChange?: (change:
    | { kind: "snapshot"; sessionId: string; items: TodoSnapshotItem[] }
    | { kind: "group_started"; sessionId: string; groupId: string; groupTitle: string | null; messageId: string | null }
    | { kind: "upsert"; sessionId: string; item: TodoSnapshotItem }
    | { kind: "remove"; sessionId: string; id: string }
  ) => void;
  /**
   * Server may request explicit user approval before running a high-risk tool.
   * Resolves with `{ decision: "approved" | "denied" }` — these values match
   * the server's `WsClientMessage::ApprovalResponse::decision` check in
   * `hermix-server/src/lib.rs::handle_ws_stream` (which compares against
   * `"approved"` literally).
   * If absent, the caller cannot respond and the request times out server-side.
   */
  onApprovalRequest?: (req: {
    requestId: string;
    tool: string;
    args: unknown;
    riskLevel?: string;
    /** Wall-clock deadline (Unix ms) when the server will auto-deny
     *  this request. Drives the front-end countdown UI. `null` /
     *  `undefined` for pre-servers that predate the
     *  countdown feature — the card falls back to a no-countdown
     *  state in that case. */
    expiresAt?: number | null;
    /** LLM-supplied one-line rationale (terminal schema `reason`),
     *  surfaced verbatim on the approval card. Absent for models that
     *  don't fill it or non-terminal tools. */
    reason?: string;
    /** The chat session this request belongs to. The server doesn't tag
     *  it on the wire, so `useStreamChat` adds it from the local closure
     *  that was passed to `send()`. The approval card manager uses this
     *  to render the inline card in the right message stream. */
    sessionId: string;
  }) => Promise<{ decision: "approved" | "denied" | "expired" }>;
  /**
   * The agent called the `clarify` tool — it is now blocked waiting for
   * the user to answer the question. Resolves with the user's free-form
   * answer (or the choice they picked). The server feeds this back to
   * the tool call as the result.
   */
  onClarifyRequest?: (req: {
    requestId: string;
    question: string;
    choices: string[];
    sessionId: string;
  }) => Promise<{ answer: string }>;
  onDone?: (fullText: string, stopReason?: string, durationMs?: number) => void;
  /**
   * Terminal error. `detail` carries the raw, un-localized backend
   * error (e.g. an LLM 400 "invalid function arguments…") so the UI can
   * show it as a collapsible "技术详情" subtext instead of dropping it.
   */
  onError?: (error: string, detail?: string, kind?: string) => void;
  /**
   * Live reasoning text from the LLM (`<think>...</think>` block),
   * stripped from the user-visible content by `ThinkBlockParser` and
   * forwarded by the server as `StreamEvent::Thinking` chunks.
   *
   * The hook forwards each chunk as it arrives (no batching) so the
   * thinking indicator in the UI can pulse in real time. The caller
   * is responsible for accumulating into the target message's
   * `reasoningContent` field.
   */
  onReasoningChunk?: (piece: string) => void;
  /**
   * (C3 marker protocol): the backend's `SectionGate`
   * ran on `resolve_initial_system_prompt` and forwarded the
   * resulting `SectionGateReport` to the `on_section_gate`
   * callback, which serialised it as a `section_gate` WS event.
   * We forward the parsed report here so the caller (ChatView)
   * can append a `section_gate` entry to the execution timeline
   * and render the per-section chips using `Section::label()`.
   *
   * `patched` lists sections the gate auto-spliced in (i.e. the
   * upstream code path *would* have skipped them silently);
   * `pending` lists optional sections absent under the current
   * `RouteContext` and are surfaced for diagnostics only.
   */
  onSectionGate?: (info: {
    patched: Array<{
      section: "identity" | "tool_list" | "tool_enforce" | "working_dir" | "timestamp";
      applied: boolean;
      reason: string;
    }>;
    pending: Array<{
      section: "identity" | "tool_list" | "tool_enforce" | "working_dir" | "timestamp";
      applied: boolean;
      reason: string;
    }>;
  }) => void;
  /**
   * H3: The prefetcher resolved with a user-intent prediction and
   * ran safe tools speculatively. Sent before LLM reasoning starts.
   */
  onPrefetch?: (info: {
    intent: string;
    confidence: number;
    results: PrefetchResultEntry[];
  }) => void;
  /**
   * M5: The agent emitted an ACUI (Agent-Controlled UI) directive.
   *
   * The wire shape is:
   *   { "type": "acui", "directive": { "op": "show"|"update"|"hide", ... } }
   *
   * The hook deserializes the `directive` field as `AcuiDirective` and
   * passes it here. Caller is responsible for evolving its own state
   * (typically via `applyAcuiDirective`).
   */
  onAcuiDirective?: (directive: AcuiDirective) => void;
  /**
   * 用户点击「停止」（cancel）时触发——在流层关闭 WS **之前**调用，
   * 让调用方有机会否决仍在途的审批/澄清请求（直接把 denied / 空答案
   * 发往后端），使后端 agent loop 立即解除阻塞，而不是干等其 300s
   * 审批超时。若未提供，cancel 仍正常收尾，只是后端靠超时兜底。
   */
  onCancel?: () => void;
}

export function useStreamChat(activeSessionId: string | null) {
  // Per-session streaming state. We track the *session id* (not just a
  // boolean) so the UI can scope the "正在思考…" indicator and the
  // composer-disabled state to the specific session that's mid-stream.
  // A global `boolean` was the previous design and it had two
  // pathologies:
  // 1. Switching to another session mid-stream would leak the
  // "thinking" state into the new session's ChatView (false
  // positive — the new session isn't actually streaming).
  // 2. After `cancel()` closed the active WS, React batching of the
  // state update could let the Composer stay disabled across
  // session switches, blocking new sends until a full
  // re-render cycle.
  // The id-typed state fixes both: ChatView only shows the indicator
  // when `streamingSessionId === activeSessionId`, and `null` is the
  // unambiguous "no stream in flight" sentinel.
  const [streamingSessionId, setStreamingSessionId] = useState<string | null>(null);
  // synchronous mirror of `streamingSessionId`. The stable
  // `send` callback closes over the INITIAL state value, so it can't
  // read the live `streamingSessionId` inside `dedup`. This ref lets the
  // dedup check ask "is a stream currently in flight for this session?"
  // without re-creating the callback.
  const streamingSessionIdRef = useRef<string | null>(null);
  // Transient status text from the backend (e.g. "Still waiting for the model…").
  const [streamingStatus, setStreamingStatus] = useState<string | null>(null);
  // Compaction pipeline status (Day 1-B placeholder). In Day 2 the backend
  // will emit a separate `compaction_status` WS event kind and we'll
  // parse it here. For now it's just a state slot wired through so
  // ChatView can render the banner without further plumbing work later.
  const [compactionStatus, setCompactionStatus] =
    useState<import("../components/CompactionStatusBanner").CompactionStatus | null>(null);
  // context window usage. Updated every turn via the
  // `context_usage` WS event. Displayed as a small bar in the chat
  // footer so the user always knows how much context budget remains.
  const [contextUsage, setContextUsage] =
    useState<{ usedTokens: number; maxTokens: number; thresholdTokens: number; percent: number } | null>(null);
  // per-turn token usage snapshot — pushed by the
  // backend as a `usage_update` WS event after every successful LLM
  // response. Drives the live "cache hit %" chip next to the
  // context-usage bar. Accumulated session-wide so the chip shows
  // a stable running rate instead of jittering every turn.
  const [usageSnapshot, setUsageSnapshot] =
    useState<{
      promptTokens: number;
      completionTokens: number;
      cacheReadTokens: number;
      cacheCreationTokens: number;
      reasoningTokens: number;
    } | null>(null);
  // Timestamp of the most recent "agent is doing real work" event.
  // Updated on text/thinking/tool/file/memory/todo/sub-agent events and
  // reset when the turn truly ends (settle). ChatView uses this to keep
  // the bottom "working" indicator alive during silent gaps where the
  // LLM is not streaming but tools/file changes are still in flight.
  const [lastActivityAt, setLastActivityAt] = useState<number | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  // type-B observability + direct-send: send a raw WS text
  // frame on the live socket. Returns false (no-op) if the socket is
  // missing or not OPEN, so callers can distinguish "sent" from
  // "dropped". Used by the approval flow so the click handler can
  // emit `approval_response` directly (decoupled from the
  // resolver/Promise round-trip) AND so we can route diagnostics to
  // the server log (the webview console is invisible in Tauri).
  const wsSendRaw = useCallback((text: string): boolean => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return false;
    try {
      ws.send(text);
      return true;
    } catch {
      return false;
    }
  }, []);
  // Refs to hold the per-send promise resolvers + callbacks,
  // so cancel() can settle the promise immediately without waiting
  // for the async WS onclose event.
  const pendingResolveRef = useRef<((value: string) => void) | null>(null);
  const pendingRejectRef = useRef<((error: Error) => void) | null>(null);
  const pendingCallbacksRef = useRef<StreamCallbacks | null>(null);
  const disarmTimeoutsRef = useRef<(() => void) | null>(null);
  // (storage-analyzer stale_stream bug fix): while a tool is
  // executing on the server (between `tool_start` and `tool_complete`),
  // suspend the STALE_STREAM_TIMEOUT countdown. spawn_blocking tools like
  // storage_analyzer can take 60-90s on big trees without emitting any
  // WS event — without this guard, the 90s idle-watchdog would
  // prematurely settle as "stale_stream" and disconnect the WS even
  // though the server is still working. IDLE_TIMEOUT_MS (300s) remains
  // the ultimate safety net for a truly hung agent loop.
  const toolRunningRef = useRef(false);
  // Tracks whether the current send's promise has already been settled.
  // We use a ref (not a local var) so cancel() can read it across
  // closure boundaries without needing access to `send`'s internals.
  const sendSettledRef = useRef(false);
  // Holds the `settle` function of the current `send` call so
  // `cancel()` can invoke it directly (synchronously resolving the
  // promise) instead of waiting for the async WS `onclose` event.
  const settleRef = useRef<
    | ((
        text: string,
        err?: string,
        reason?: string,
        phaseHint?: StreamPhase,
        detail?: string,
        keepSessionAlive?: boolean,
        kind?: string,
      ) => void)
    | null
  >(null);
  // Client-side dedup: track last sent (hash, timestamp) to prevent
  // the same content from being sent multiple times within the window.
  const lastSendRef = useRef<{ hash: string; ts: number } | null>(null);
  // Per-send isolation via a monotonically increasing ID.
  // Each `send()` call increments `activeSendIdRef.current` and captures
  // its own `mySendId`. All WS event handlers (onmessage, onerror, onclose)
  // and the `settle` function check `activeSendIdRef.current === mySendId`
  // before proceeding. This prevents a stale WS's `onclose` (which fires
  // asynchronously after `wsRef.current?.close()`) from interfering with
  // a new `send()` that has already reset `doneRef` and swapped
  // `wsRef.current` to a new WebSocket.
  // The race condition this fixes:
  // 1. send1's onDone → settle1() → close WS1 → resolve promise
  // 2. Queue drain fires → send2 starts → doneRef=false, wsRef=WS2
  // 3. WS1's onclose fires (async macro task) → settle1() runs again
  // → closes wsRef.current (WS2!) → send2 shows "disconnected"
  const activeSendIdRef = useRef(0);
  // track the currently-active session id so per-session
  // context_usage / usage_update events can be filtered — drop stale
  // events from a background session once the user switches away.
  const activeSessionIdRef = useRef<string | null>(activeSessionId);
  useEffect(() => { activeSessionIdRef.current = activeSessionId; }, [activeSessionId]);
  // Session id of the WS connection currently held in wsRef.current.
  const wsSessionIdRef = useRef<string | null>(null);
  // shared per-send state via refs so that WS reconnection
  // (doConnect retry) shares the same `done` / `sentToServer` flags
  // instead of creating fresh local variables each time.
  const doneRef = useRef(false);
  const sentToServerRef = useRef(false);
  // Per-send wall-clock start. Captured at the moment the
  // user fires `send()` (i.e. the assistant turn begins). Read at
  // `onDone` to compute `durationMs` and pass it on to App.tsx, where
  // it's stamped onto the assistant message. Reloaded history has no
  // value here (this ref is per-session, lost on reload) — those
  // messages render without a duration by design.
  const sendStartedAtRef = useRef<number | null>(null);
  // Holds the per-send `callbacks.onPrefetch` closure so the WS
  // message handler (line 407) can fire it without prop-drilling the
  // whole `callbacks` object through closures. Ref (not state) because
  // we only need synchronous read access from the WS callback — never
  // re-rendering on a fresh closure is a feature, not a bug.
  const onPrefetchRef = useRef<((info: {
    intent: string;
    confidence: number;
    results: PrefetchResultEntry[];
  }) => void) | undefined>(undefined);

  // Defensive cleanup: if the component unmounts while a stream is
  // still in flight, close the WS so we don't leak a connection that
  // would keep trying to call `setStreamingSessionId` on a dead
  // component (React 19 will warn about that, and the promise would
  // never settle — the user's previous send would hang forever).
  useEffect(() => {
    return () => {
      console.log("[WS-CLOSE] site=unmount (组件卸载时仍有在途流，主动关闭 WS)");
      wsRef.current?.close();
      wsRef.current = null;
    };
  }, []);

  const send = useCallback(
    (
      sessionId: string,
      content: string,
      callbacks: StreamCallbacks = {},
      overrides: ChatOverrides = {},
    ): Promise<string> => {
      // Register the per-send onPrefetch so the WS callback can read it.
      onPrefetchRef.current = callbacks.onPrefetch;

      return new Promise((resolve, reject) => {
        // Expose the raw resolvers so cancel() can settle the promise
        // immediately without waiting for the async WS onclose event.
        pendingResolveRef.current = resolve;
        pendingRejectRef.current = reject;
        pendingCallbacksRef.current = callbacks;
        sendSettledRef.current = false;
        // Mark the start of this assistant turn so `onDone`
        // can compute how long the LLM took. Set BEFORE dedup so a
        // deduped call doesn't pollute the timing of the next real
        // send. (A deduped call resolves immediately without firing
        // the WS, so this ref would be overwritten on the next real
        // send anyway — but explicit reset keeps the invariant local.)
        sendStartedAtRef.current = Date.now();
        setLastActivityAt(Date.now());

        // ── Client-side message dedup (runs BEFORE setStreamingSessionId) ──
        // A duplicate must resolve("") cleanly instead of `return`-ing from
        // inside this Promise executor: an un-resolved promise would hang
        // `await sendStream(...)` forever and leave `streaming` stuck true,
        // so the next Enter is routed into the message queue instead of
        // sending — looping identical content to the backend. Coalesce only
        // same-content re-sends WHILE this session is actively streaming;
        // after the stream settles, a re-send is a deliberate retry and must
        // pass through to the backend.
        const DEDUP_WINDOW_MS = 15_000;
        const contentHash = `${sessionId}:${content}`;
        const now = Date.now();
        const __sendDiagId = nextDiagId();
        diagLog(`[diag #${__sendDiagId}] sendStream ENTER`, { sessionId, content, lastSend: lastSendRef.current, now });
        if (
          lastSendRef.current &&
          lastSendRef.current.hash === contentHash &&
          now - lastSendRef.current.ts < DEDUP_WINDOW_MS &&
          // only coalesce a same-content re-send WHILE this
          // session is actively streaming. Once the stream has settled
          // (LLM self-interrupted / user stopped / stale-timeout),
          // `streamingSessionId` is cleared, so a re-send of the
          // original question is a deliberate retry and must pass
          // through to the backend — which now takes over any stale
          // in-flight task instead of dropping the send as a "duplicate".
          streamingSessionIdRef.current === sessionId
        ) {
          diagWarn(
            `[useStreamChat #${__sendDiagId}] dedup: same content sent ${now - lastSendRef.current.ts}ms ago, skipping`,
          );
          resolve("");
          return;
        }
        lastSendRef.current = { hash: contentHash, ts: now };

        // Increment the active send ID so any stale WS
        // event handlers from a previous `send()` call become no-ops.
        // The previous send should have already settled (via the `await`
        // and queue mechanism in App.tsx), but this is a safety net for
        // edge cases where the old WS's `onclose` fires asynchronously.
        const mySendId = ++activeSendIdRef.current;

        streamingSessionIdRef.current = sessionId;
        setStreamingSessionId(sessionId);
        let fullText = "";
        doneRef.current = false;
        sentToServerRef.current = false;
        let stopReason: string | undefined;
        // ── Per-send chunk batching ─────────────────────────────
        // LLM streaming emits 30-50 small text frames per second.
        // Forwarding each one to the React state setter (via
        // callbacks.onChunk → App.appendChunk → setSessions) triggers
        // 30-50 React commits per second, each one walking the full
        // sessions×messages tree (`prev.map`/`s.messages.map`) and
        // re-rendering the whole App subtree. On a long conversation
        // this is the dominant CPU sink while streaming.
        // Collapse N chunks into one batch per rAF tick (≤16ms on a
        // 60Hz display). The "正在打字…" cursor still animates every
        // frame; we just stop forcing a full React reconciliation on
        // every token. The on-hook `fullText` accumulator is still
        // updated synchronously so `onDone` (settle) gets the full
        // server text regardless of when rAF last fired.
        // Scoped to the `send` closure (not module-level refs) so a
        // second `send` call can't accidentally fire a stale rAF with
        // the previous send's `callbacks` closure.
        let chunkBuffer = "";
        let chunkRaf: number | null = null;
        const flushChunks = () => {
          chunkRaf = null;
          const batched = chunkBuffer;
          chunkBuffer = "";
          if (batched) callbacks.onChunk?.(batched);
        };
        const scheduleChunkFlush = () => {
          if (chunkRaf !== null) return;
          chunkRaf = requestAnimationFrame(flushChunks);
        };

        // ── Per-send retry state ─────────────────────────────────
        // auto-retry when the WS fails before any content
        // has arrived. After the first chunk, we settle immediately
        // (no retry — the agent loop is already running and duplicating
        // it would produce duplicate tool results).
        const MAX_RETRIES = 3;
        let retryCount = 0;
        const RETRY_DELAYS_MS = [500, 1000, 2000];

        // ── Phase tracking ───────────────────────────────────────
        // 30s first-chunk timeout → `slow` (UI hint, stream still alive)
        // 120s total timeout → `timeout` (genuine abandonment)
        // `done`/`error`/WS-close → terminal phase (`completed` / `error` / `partial`)
        let phase: StreamPhase = "connecting";
        const updatePhase = (next: StreamPhase) => { phase = next; callbacks.onPhaseChange?.(next); };
        let firstChunkReceived = false;
        let hasContent = false;
        // Tracks whether *any* non-status stream event has arrived
        // (text, thinking, tool_start, tool_complete, file_change,
        // memory_update, memory_recall, todo, sub_agent, etc.). This is
        // distinct from `hasContent` which only counts user-visible text.
        // Used by the WS onclose handler to decide "partial" vs "error"
        // when the socket drops mid-stream: a turn that only produced
        // reasoning/thinking should still be treated as a partial result,
        // not a connection error that silently clears the streaming state
        // and shows "可以继续提问" while the user still sees thinking output.
        let hasAnyStreamEvent = false;
        // true once `ws.send(payload)` succeeds in the
        // `connected` handler. If the WS closes before any content
        // arrives but AFTER the message was sent to the server, we
        // MUST NOT retry — the server already received it and will
        // eventually respond (or complete). Retrying with a fresh WS
        // sends the same content again, creating a second agent loop
        // and two assistant replies in the DB — visible to the user
        // as "重复回复".
        sentToServerRef.current = false;
        let idleTimeout: ReturnType<typeof setTimeout> | null = null;
        // 30s first-chunk slow-hint timer handle (see ws.onmessage "connected"
        // branch). Tracked so settle()/disarmTimeouts() can cancel it — without
        // a handle, a stale 30s timer can fire after the stream already
        // terminated, flipping `phase` back to "slow" right after the user
        // already saw "completed".
        let slowHintTimer: ReturnType<typeof setTimeout> | null = null;
        // The idle timeout (300s) resets on every received WS event (text,
        // thinking, tool_start, tool_complete, status, …), so long-running
        // tasks that are actively streaming stay alive; it only fires when
        // the agent goes silent.
        // A separate STALE timeout (also 300s) catches the backend agent
        // loop exiting silently — e.g. Finalize without sending Done (a
        // MiniMax empty-tool-call dead-end) — so the stream settles as
        // "partial" instead of freezing on "thinking…". Both timers are
        // suspended while a tool is in flight (toolRunningRef) so the
        // watchdog can't fire mid-execution; the only way STALE triggers is
        // a real hang: no tool in flight and no text for 5 minutes.
        const IDLE_TIMEOUT_MS = 300_000;
        const STALE_STREAM_TIMEOUT_MS = 300_000;
        let staleStreamTimer: ReturnType<typeof setTimeout> | null = null;
        // When an approval card is pending, the agent loop is blocked waiting
        // for the user's click and no WS events arrive, so the idle/stale
        // timers would fire and settle the stream as "timeout" / "partial" —
        // aborting the approval wait. This flag suspends both timers while an
        // approval is in flight; it's set when `request_approval` arrives and
        // cleared when the user clicks.
        let approvalPending = false;

        const armTimeouts = () => {
          resetIdleTimeout();
          resetStaleTimer();
        };
        // Reset the IDLE timer (300s). Called on every received WS event.
        // If no event arrives within IDLE_TIMEOUT_MS, the stream is
        // considered abandoned and we settle with "timeout".
        const resetIdleTimeout = () => {
          if (idleTimeout !== null) clearTimeout(idleTimeout);
          idleTimeout = setTimeout(() => {
            if (!doneRef.current) {
              updatePhase("timeout");
              // Force-settle so the promise doesn't hang forever.
              // Without this the UI stays stuck in "timeout" phase
              // with `sending=true` indefinitely.
              settleRef.current?.("", undefined, "timeout", "timeout");
            }
          }, IDLE_TIMEOUT_MS);
        };
        // Reset the STALE timer (300s). When a tool is in flight
        // (toolRunningRef.current === true), we use a longer cap
        // (STALE_STREAM_TIMEOUT_MS * 2 = 600s) instead of completely
        // disabling the timer. This prevents a hung tool from keeping
        // the stream alive forever — if the backend crashes mid-tool
        // (no tool_complete, no Done event), the 600s cap ensures the
        // user doesn't stare at "正在思考" for 5+ minutes.
        // The timer is re-armed by `tool_complete` (which flips the ref
        // back to false) and by every text/thinking event afterwards.
        const TOOL_STALE_CAP_MS = STALE_STREAM_TIMEOUT_MS * 2;
        const resetStaleTimer = () => {
          if (staleStreamTimer !== null) clearTimeout(staleStreamTimer);
          const timeout = toolRunningRef.current
            ? TOOL_STALE_CAP_MS
            : STALE_STREAM_TIMEOUT_MS;
          staleStreamTimer = setTimeout(() => {
            if (!doneRef.current) {
              // Don't override the phase if already in a settled state.
              settleRef.current?.(
                "", // preserve text we already have
                undefined,
                "stale_stream",
                "partial"
              );
            }
          }, timeout);
        };
        const disarmTimeouts = () => {
          if (idleTimeout !== null) {
            clearTimeout(idleTimeout);
            idleTimeout = null;
          }
          if (staleStreamTimer !== null) {
            clearTimeout(staleStreamTimer);
            staleStreamTimer = null;
          }
          if (slowHintTimer !== null) {
            clearTimeout(slowHintTimer);
            slowHintTimer = null;
          }
          // Clear the tool-in-flight flag so the next send() starts
          // with a clean stale-stream watchdog state.
          toolRunningRef.current = false;
        };
        disarmTimeoutsRef.current = disarmTimeouts;

        const settle = (
          text: string,
          err?: string,
          reason?: string,
          phaseHint?: StreamPhase,
          detail?: string,
          keepSessionAlive?: boolean,
          kind?: string,
        ) => {
          if (doneRef.current) return;
          // Guard against stale WS event handlers. If a new
          // `send()` has started (incrementing `activeSendIdRef`), this
          // `settle` belongs to an old send and must be a no-op —
          // otherwise it would close the new WS and corrupt the new
          // send's state.
          if (activeSendIdRef.current !== mySendId) return;
          // Turn is truly ending: clear the activity beacon so the
          // bottom indicator can flip to "可以继续提问" immediately.
          setLastActivityAt(null);
          disarmTimeouts();
          // Drain any chunks that arrived after the last rAF tick
          // (typical: the "done" event follows a final burst of
          // text frames in the same microtask). Cancel the rAF first
          // so its in-flight callback can't fire *after* `onDone` and
          // re-render stale text on top of the canonical terminal.
          if (chunkRaf !== null) {
            cancelAnimationFrame(chunkRaf);
            chunkRaf = null;
          }
          const pending = chunkBuffer;
          chunkBuffer = "";
          if (pending) callbacks.onChunk?.(pending);

          doneRef.current = true;
          // keepSessionAlive (backend outcome != "completed"): a backend
          // failure / invalid-tool exhaustion / unfinished turn is NOT a
          // session death. Keep the WS open and the session id so a
          // pending approval or a user retry can still resume — only a
          // clean `completed` (or an explicit cancel/unmount) tears down
          // the socket. Without this, a failed tool call silently closed
          // the connection and dropped any in-flight approval.
          if (keepSessionAlive) {
            console.log(`[WS-KEEPALIVE] site=settle reason=${reason ?? "interrupted"} (outcome!=completed, session preserved)`);
          } else {
            streamingSessionIdRef.current = null;
            setStreamingSessionId(null);
            setStreamingStatus(null);
            console.log(`[WS-CLOSE] site=settle reason=${reason ?? "natural-done"}`);
            wsRef.current?.close();
            wsRef.current = null;
          }
          if (reason) stopReason = reason;
          // Emit terminal phase
          updatePhase(phaseHint ?? (err ? "error" : "completed"));

          // Strip leaked status/activity markers from final text
          // (defense-in-depth: old backend may have injected these
          // as Text events before the lib.rs fix was compiled).
          const cleanText = text.replace(/\n?\[(?:activity|status):[^\]]*\]\s*/g, "");

          if (err) {
            callbacks.onError?.(err, detail, kind);
            // `pendingRejectRef` holds the raw `reject` (no mutex wrap).
            // Fall back to the local `reject` if the ref has already
            // been nulled (e.g. on a second settle call).
            (pendingRejectRef.current ?? reject)(new Error(err));
          } else {
            // Measure how long this assistant turn took
            // (Date.now() at send() → Date.now() at Done). Sent to
            // App.tsx so it can stamp the duration onto the message.
            // Clamp at 0 to defend against clock skew (send before
            // now() somehow — should not happen but cheap to guard).
            const durationMs = sendStartedAtRef.current !== null
              ? Math.max(0, Date.now() - sendStartedAtRef.current)
              : undefined;
            callbacks.onDone?.(cleanText, stopReason, durationMs);
            (pendingResolveRef.current ?? resolve)(cleanText);
          }
          // Clean up per-send refs so a stale cancel() call
          // can't settle an already-settled promise.
          pendingResolveRef.current = null;
          pendingRejectRef.current = null;
          pendingCallbacksRef.current = null;
          sendStartedAtRef.current = null;
          sendSettledRef.current = true;
          settleRef.current = null;
        };
        settleRef.current = settle;

        // extracted WS creation into a callable so the
        // auto-retry path (before any content arrives) can create a
        // fresh WS without duplicating the ~200 lines of handler setup.
        const doConnect = () => {
        // Stale retry guard — if a new `send()` has started
        // during the retry delay, abort the reconnection attempt so we
        // don't overwrite the new send's WS in `wsRef.current`.
        if (activeSendIdRef.current !== mySendId) return;
        try {
          const ws = new WebSocket(resolveWebSocketUrl(sessionId));
          wsRef.current = ws;
          wsSessionIdRef.current = sessionId;

          // Approval responses are routed back to the original
          // `callbacks.onApprovalRequest(...)` Promise through a closure:
          // the request id flows in the WS frame and the corresponding
          // resolver lives in `App.tsx` via `approvalResolversRef`. Stale
          // resolver cleanup therefore happens in App.tsx when a stream
          // settles or the session changes — not here.

          ws.onmessage = (evt) => {
            // Stale WS guard — if a new send has started,
            // ignore events from the old WS.
            if (activeSendIdRef.current !== mySendId) return;
            // Reset the idle timeout on every received
            // event. As long as the agent is actively producing data
            // (text, thinking, tool events, status), the stream stays
            // alive. Only when no event arrives for IDLE_TIMEOUT_MS
            // does the timeout fire.
            // Skip the idle reset while an approval is pending — the agent is
            // blocked waiting for the user's click, so no WS events arrive and
            // the timer would otherwise fire after 300s and kill the wait.
            if (approvalPending) {
              // Still clear any existing timer so it doesn't fire
              // from before the approval was pending.
              if (idleTimeout !== null) {
                clearTimeout(idleTimeout);
                idleTimeout = null;
              }
            } else if (idleTimeout !== null) {
              clearTimeout(idleTimeout);
              idleTimeout = setTimeout(() => {
                if (!doneRef.current) {
                  updatePhase("timeout");
                  settleRef.current?.("", undefined, "timeout", "timeout");
                }
              }, IDLE_TIMEOUT_MS);
            }
            // Arm the stale timer on every WS event (90s of "no text" before
            // auto-settle; tool-execution pauses don't extend it). Skip while
            // an approval is pending — same reasoning as the idle guard above.
            if (!approvalPending) resetStaleTimer();
            try {
              const data = JSON.parse(String(evt.data));

              // Connected event — send the user message + per-request overrides
              if (data?.type === "connected" || data?.event?.type === "connected") {
                const payload: Record<string, unknown> = {
                  text: content,
                  request_id: `req-${Date.now()}`,
                };
                if (overrides.workspace) payload.workspace = overrides.workspace;
                if (overrides.systemPromptOverride)
                  payload.system_prompt = overrides.systemPromptOverride;
                if (overrides.tools && overrides.tools.length > 0) {
                  payload.tools = overrides.tools;
                }
                // selectedModel (e.g. "minimax:MiniMax-M2.7") must reach the
                // backend so `effective_model` switches to the user's chosen
                // provider; if unset, the backend uses config.yaml's default.
                if (overrides.model) payload.model = overrides.model;
                if (overrides.personality) payload.personality = overrides.personality;
                if (overrides.permissionLevel) payload.permission = overrides.permissionLevel;
                if (overrides.skills && overrides.skills.length > 0) {
                  payload.skills = overrides.skills;
                }
                if (overrides.image) payload.image = overrides.image;
                if (overrides.imageMimeType) payload.image_mime_type = overrides.imageMimeType;
                if (overrides.images && overrides.images.length > 0) {
                  payload.images = overrides.images;
                }
                ws.send(JSON.stringify(payload));
                sentToServerRef.current = true;
                const __sendDiagId2 = nextDiagId();
                const __sendDiagTs2 = performance.now().toFixed(1);
                diagLog(`[diag #${__sendDiagId2} @${__sendDiagTs2}ms] WS connected → ws.send(payload) FIRED, retryCount=${retryCount}, textLen=${content.length}, sessionId=${sessionId}`);
                updatePhase("streaming");
                armTimeouts();

                // 30s slow-hint timer: if first chunk hasn't arrived by then,
                // the stream is still alive (WS open, no done/error yet).
                // Handle is tracked so settle()/disarmTimeouts() can cancel it.
                slowHintTimer = setTimeout(() => {
                  if (!firstChunkReceived && !doneRef.current) updatePhase("slow");
                }, 30_000);
                return;
              }

              // Streaming text
              const evtType = data?.type || data?.event?.type;
              const evtContent = data?.content || data?.event?.content;

              if (evtType === "text") {
                // Reset the stale-stream timer on every text chunk
                // so tool-only turns (no user-visible output) don't
                // keep the stream alive indefinitely.
                resetStaleTimer();
                firstChunkReceived = true;
                if (phase === "slow") updatePhase("streaming");
                const piece = typeof evtContent === "string" ? evtContent : "";
                // Defense-in-depth: if the old backend still injects
                // [activity: …] / [status: …] as Text events (before
                // the lib.rs fix was compiled), silently drop them so
                // they never pollute message body.
                if (piece && !/^\[(?:activity|status):\s*/.test(piece)) {
                  hasContent = true;
                  hasAnyStreamEvent = true;
                  setLastActivityAt(Date.now());
                  fullText += piece;
                  chunkBuffer += piece;
                  scheduleChunkFlush();
                }
                return;
              }

              // Live reasoning text (server emits `StreamEvent::Thinking`
              // chunks as the LLM produces its `<think>...</think>`
              // monologue). Forward raw to caller — caller accumulates
              // into `message.reasoningContent`. We intentionally do
              // NOT merge into `fullText` (the visible reply) and do
              // NOT count toward `hasContent` (the done-event check).
              if (evtType === "thinking") {
                const piece = typeof evtContent === "string" ? evtContent : "";
                if (piece) {
                  hasAnyStreamEvent = true;
                  setLastActivityAt(Date.now());
                  callbacks.onReasoningChunk?.(piece);
                }
                return;
              }

              if (evtType === "tool_start") {
                const tool = data?.tool || data?.event?.tool || "tool";
                const toolCallId =
                  data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
                toolRunningRef.current = true;
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                callbacks.onToolStart?.({ toolCallId, tool, content: evtContent });
                return;
              }

              if (evtType === "tool_complete") {
                const tool = data?.tool || data?.event?.tool || "tool";
                const toolCallId =
                  data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
                // (A+C product fix): a user-facing denial
                // (user拒绝 / 超时 / 连接中断) carries `denial_kind`.
                // Forward it so App.tsx can show a "this call was
                // blocked" banner that matches the agent's injected
                // "tell the user" note. `"policy"` is a silent gate
                // and is intentionally ignored here.
                const denialKind =
                  data?.denial_kind || data?.event?.denial_kind || undefined;
                // Flip toolRunningRef back to false BEFORE resetStaleTimer
                // so the watchdog gets re-armed for a fresh STALE window.
                toolRunningRef.current = false;
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                resetStaleTimer();
                callbacks.onToolComplete?.({ toolCallId, tool, content: evtContent, denialKind });
                return;
              }

              if (evtType === "sub_agent_event") {
                // Live sub-agent lifecycle event. The server serialises
                // it as `WsEnvelope.event` with `type: "sub_agent_event"`
                // and the fields on `data.event`. Forward to the caller
                // so the "子代理" panel can update the matching row.
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data;
                callbacks.onSubAgentEvent?.({
                  subAgentId: ev?.sub_agent_id,
                  phase: ev?.phase,
                  task: typeof ev?.task === "string" ? ev.task : "",
                  role: ev?.role,
                  turns: typeof ev?.turns === "number" ? ev.turns : undefined,
                  costUsd: typeof ev?.cost_usd === "number" ? ev.cost_usd : undefined,
                  error: ev?.error,
                  // Rolling step log (tool calls / thinking), newest last.
                  // Mirrors the backend `recent_steps` array. May be
                  // absent on older servers — panel renders without it.
                  recentSteps: Array.isArray(ev?.recent_steps)
                    ? ev.recent_steps.filter((s: unknown) => typeof s === "string")
                    : undefined,
                });
                return;
              }

              if (evtType === "tool_chunk") {
                const tool = data?.tool || data?.event?.tool || "tool";
                const toolCallId =
                  data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
                const chunkIndex =
                  typeof (data?.chunk_index ?? data?.event?.chunk_index) === "number"
                    ? data.chunk_index ?? data.event.chunk_index
                    : 0;
                const chunkTotal =
                  typeof (data?.chunk_total ?? data?.event?.chunk_total) === "number"
                    ? data.chunk_total ?? data.event.chunk_total
                    : 1;
                const content =
                  typeof (data?.content ?? data?.event?.content) === "string"
                    ? data.content ?? data.event.content
                    : "";
                if (content.length > 0) {
                  hasAnyStreamEvent = true;
                  setLastActivityAt(Date.now());
                  callbacks.onToolChunk?.({
                    toolCallId,
                    tool,
                    content,
                    chunkIndex,
                    chunkTotal,
                  });
                }
                return;
              }

              // File-change summary (WorkBuddy-style "文件变更" row).
              // Server emits this after a successful write_file / patch.
              // We pull fields from either the top-level payload or
              // `event.*` to be tolerant of the two WS envelope shapes.
              if (evtType === "file_change") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const filePath = ev?.file_path ?? ev?.filePath ?? "";
                if (!filePath) return;
                const rawType = ev?.change_type ?? ev?.changeType ?? "modified";
                const allowed = ["created", "modified", "deleted", "renamed"];
                const changeType = allowed.includes(rawType)
                  ? (rawType as "created" | "modified" | "deleted" | "renamed")
                  : "modified";
                const toolCallId =
                  ev?.tool_call_id ?? ev?.toolCallId ?? `file_change::${filePath}`;
                const fileSummary = ev?.file_summary ?? ev?.fileSummary ?? undefined;
                callbacks.onFileChange?.({
                  toolCallId,
                  filePath,
                  changeType,
                  fileSummary,
                });
                return;
              }

              // Memory mutation surfaced inline (WorkBuddy-style
              // "记忆" row). Server emits this after the agent writes /
              // updates / deletes / summarizes memory so the user can
              // see what was remembered. Mirrors the `file_change`
              // branch shape.
              if (evtType === "memory_update") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const action = (ev?.action ?? ev?.memory_action ?? "write") as
                  | "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
                const memoryType = (ev?.memory_type ?? ev?.memoryType ?? "fact") as
                  | "fact" | "style" | "summary" | "tool" | "snapshot" | "reflection";
                const key = ev?.key ?? ev?.memory_key ?? "";
                const summary = ev?.summary ?? ev?.memory_summary ?? "";
                const count = ev?.count ?? ev?.memory_count ?? undefined;
                if (!summary && !key) return;
                callbacks.onMemoryUpdate?.({
                  action,
                  memoryType,
                  key,
                  summary,
                  count,
                });
                return;
              }

              // recall event: fires when the agent
              // re-pulls related facts from memory this turn. We
              // forward to onMemoryRecall so App.tsx can render a
              // small "💭 想到 N 条" chip in the conversation.
              if (evtType === "memory_recall") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const trigger = (ev?.trigger ?? "interval") as
                  | "first_turn" | "interval" | "drift";
                const count = Number(ev?.count ?? 0);
                const totalChars = ev?.total_chars ?? ev?.totalChars ?? undefined;
                const previewKeys: string[] = Array.isArray(ev?.preview_keys ?? ev?.previewKeys)
                  ? (ev?.preview_keys ?? ev?.previewKeys)
                  : [];
                const turnNumber = Number(ev?.turn_number ?? ev?.turnNumber ?? 0);
                if (count === 0) return;
                callbacks.onMemoryRecall?.({
                  trigger,
                  count,
                  totalChars,
                  previewKeys,
                  turnNumber,
                });
                return;
              }

              // Todo (task list) mutations. Wire format:
              // { type: "todo_snapshot",       event: { session_id, items: [...] } }
              // { type: "todo_group_started",  event: { session_id, group_id, group_title, message_id } }
              // { type: "todo_item_upserted",  event: { session_id, item: {...} } }
              // { type: "todo_item_removed",   event: { session_id, id: "..." } }
              // Routing is straight to onTodoChange; no message-bubble
              // side effects (the TodoListCard subscribes directly).
              // `todo_snapshot` rebuilds `todoGroups` from the authoritative
              // server state, so reload / reconnect / multi-client hydration
              // stays consistent instead of silently dropping the task list.
              if (evtType === "todo_snapshot") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const items = ev?.items ?? [];
                const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
                if (Array.isArray(items)) {
                  callbacks.onTodoChange?.({
                    kind: "snapshot",
                    sessionId,
                    items: items as TodoSnapshotItem[],
                  });
                }
                return;
              }
              if (evtType === "todo_group_started") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const groupId = ev?.group_id ?? ev?.groupId ?? "";
                const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
                const groupTitle = ev?.group_title ?? ev?.groupTitle ?? null;
                const messageId = ev?.message_id ?? ev?.messageId ?? null;
                if (groupId) {
                  callbacks.onTodoChange?.({
                    kind: "group_started",
                    sessionId,
                    groupId,
                    groupTitle,
                    messageId,
                  });
                }
                return;
              }
              if (evtType === "todo_item_upserted") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const item = ev?.item;
                const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
                if (item && item.id) {
                  callbacks.onTodoChange?.({
                    kind: "upsert",
                    sessionId,
                    item: item as TodoSnapshotItem,
                  });
                }
                return;
              }
              if (evtType === "todo_item_removed") {
                hasAnyStreamEvent = true;
                setLastActivityAt(Date.now());
                const ev = data?.event ?? data ?? {};
                const id = ev?.id ?? "";
                const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
                if (id) {
                  callbacks.onTodoChange?.({ kind: "remove", sessionId, id });
                }
                return;
              }

              // Backend status / activity notifications (e.g. "Still waiting
              // for the model…").  Displayed as a chip in StreamingBubble,
              // NOT injected into message text.
              if (evtType === "status" || evtType === "activity") {
                const content =
                  typeof evtContent === "string" ? evtContent : "";
                setStreamingStatus(content || null);
                return;
              }

              // Server is asking for explicit user approval before running a
              // high-risk tool. Forward to caller; when they answer, send the
              // response back via the same socket.
              if (evtType === "request_approval" || evtType === "RequestApproval") {
                // Suspend idle/stale timers while the approval card is shown:
                // the agent loop is blocked waiting for the user's click, so
                // no WS events arrive and the stream would otherwise settle as
                // "timeout" after 300s and kill the approval wait. Timers are
                // re-armed in the .then() / .catch() callbacks below.
                approvalPending = true;
                if (idleTimeout !== null) { clearTimeout(idleTimeout); idleTimeout = null; }
                if (staleStreamTimer !== null) { clearTimeout(staleStreamTimer); staleStreamTimer = null; }
                // The agent loop is paused waiting for the user's click, but
                // the turn's `done` event only arrives after the approval
                // resolves — so `streamingSessionId` would otherwise stay
                // set and ChatView keeps rendering a "正在思考…" bubble next
                // to the approval card. The approval card itself IS the
                // waiting state; clear the streaming placeholder now and let
                // a fresh stream-start event (if the loop continues) re-arm
                // it later.
                streamingSessionIdRef.current = null;
                setStreamingSessionId(null);
                setStreamingStatus(null);
                const ev = data?.event ?? data;
                const requestId = ev?.request_id ?? ev?.requestId;
                const tool = ev?.tool ?? "tool";
                const args = ev?.args ?? ev?.arguments;
                // LLM-supplied rationale (terminal schema `reason`),
                // surfaced on the approval card. May be absent (older
                // models / non-terminal tools) — card falls back to a
                // generic note.
                const reason = typeof ev?.reason === "string" ? ev.reason : undefined;
                // The server's `StreamEvent::RequestApproval` field is
                // `risk` (snake_case in JSON), but older drafts used
                // `risk_level` / `riskLevel`. Accept any of them.
                const riskLevel = ev?.risk_level ?? ev?.riskLevel ?? ev?.risk;
                // `expires_at` is Unix milliseconds (matches
                // `Date.now()` on the client). Older servers may
                // omit it; default to `null` so the card knows to
                // skip the countdown UI.
                const expiresAt: number | null =
                  typeof ev?.expires_at === "number" ? ev.expires_at : null;
                if (!requestId) return;
                if (callbacks.onApprovalRequest) {
                  callbacks
                    .onApprovalRequest({ requestId, tool, args, riskLevel, reason, expiresAt, sessionId })
                    .then((resp) => {
                      // Re-arm idle/stale timers — the agent loop is about
                      // to resume, so the stream watchdog must be active again.
                      approvalPending = false;
                      armTimeouts();
                      // The `expired` decision means the user
                      // clicked after the resolver was already
                      // gone (e.g. the server's 5-minute
                      // `DEFAULT_APPROVAL_TIMEOUT` had already
                      // auto-denied, or the WebSocket closed).
                      // The backend has nothing to do — it
                      // already settled this request — so we
                      // do NOT send `approval_response` over
                      // the wire. Sending one would either be
                      // a no-op (registry entry gone) or, in
                      // a different code path, might
                      // accidentally re-resolve a different
                      // pending request.
                      console.error(`[APPROVAL-THEN] firing decision=${resp.decision} id=${requestId} t=${Date.now()} wsState=${wsRef.current?.readyState}`);
                      wsSendRaw(JSON.stringify({ type: "__diag", tag: "THEN", id: String(requestId), decision: String(resp.decision), ws: wsRef.current?.readyState ?? -1 }));
                      if (resp.decision === "expired") return;
                      // The closure-captured `ws` was the WS that delivered
                      // `request_approval`, which may have been replaced by a
                      // reconnect by the time the user clicks (especially
                      // after the 30-second approval window). Always use
                      // `wsRef.current` (the live WS) and refuse to send if
                      // it's not OPEN — otherwise the click is silently
                      // discarded, the server times out at 30s, and the user
                      // sees "已允许" + "approval timed out" simultaneously.
                      const liveWs = wsRef.current;
                      if (!liveWs || liveWs.readyState !== WebSocket.OPEN) {
                        console.error(`[APPROVAL-DROP] wsRef not OPEN state=${liveWs?.readyState} id=${requestId} — dropped`);
                        wsSendRaw(JSON.stringify({ type: "__diag", tag: "DROP", id: String(requestId), state: liveWs?.readyState ?? -1 }));
                        diagWarn(
                          `[approval] wsRef.current not OPEN (state=${liveWs?.readyState}) — click for ${requestId} dropped; user must retry`,
                        );
                        return;
                      }
                      try {
                        // Server expects "approved" or "denied" (exact match
                        // against `decision == "approved"` in lib.rs:1225).
                        const decision = resp.decision === "approved" ? "approved" : "denied";
                        const payload = JSON.stringify({
                          type: "approval_response",
                          request_id: requestId,
                          decision,
                        });
                        liveWs.send(payload);
                        console.error(`[APPROVAL-SENT] ok id=${requestId} decision=${decision} payloadLen=${payload.length}`);
                        wsSendRaw(JSON.stringify({ type: "__diag", tag: "SENT", id: String(requestId), decision }));
                      } catch (e) {
                        console.error(`[APPROVAL-ERROR] send threw id=${requestId}: ${String(e)}`);
                        wsSendRaw(JSON.stringify({ type: "__diag", tag: "ERR", id: String(requestId), err: String(e) }));
                        diagWarn(
                          `[approval] liveWs.send threw for ${requestId}: ${String(e)}`,
                        );
                      }
                    })
                    .catch(() => {
                      // Re-arm idle/stale timers.
                      approvalPending = false;
                      armTimeouts();
                      // Same hardening on the deny path.
                      const liveWs = wsRef.current;
                      if (!liveWs || liveWs.readyState !== WebSocket.OPEN) return;
                      try {
                        liveWs.send(
                          JSON.stringify({
                            type: "approval_response",
                            request_id: requestId,
                            decision: "denied",
                          }),
                        );
                      } catch {}
                    });
                }
                return;
              }

              // Server is asking the user a clarification question. Render
              // a question card; when the user picks / types an answer,
              // send it back as `clarify_response`.
              if (evtType === "clarify_request" || evtType === "ClarifyRequest") {
                const ev = data?.event ?? data;
                const requestId: string | undefined =
                  ev?.request_id ?? ev?.requestId;
                const question: string = ev?.question ?? "";
                const choices: string[] = Array.isArray(ev?.choices) ? ev.choices : [];
                if (!requestId) return;
                if (callbacks.onClarifyRequest) {
                  callbacks
                    .onClarifyRequest({ requestId, question, choices, sessionId })
                    .then((resp) => {
                      // stale-click hardening — same as
                      // approval_response above. Use wsRef.current so
                      // a reconnect between `clarify_request` arrival
                      // and the user's answer doesn't silently drop
                      // the reply.
                      const liveWs = wsRef.current;
                      if (!liveWs || liveWs.readyState !== WebSocket.OPEN) return;
                      try {
                        liveWs.send(
                          JSON.stringify({
                            type: "clarify_response",
                            request_id: requestId,
                            answer: resp.answer ?? "",
                          }),
                        );
                      } catch {}
                    })
                    .catch(() => {
                      const liveWs = wsRef.current;
                      if (!liveWs || liveWs.readyState !== WebSocket.OPEN) return;
                      try {
                        liveWs.send(
                          JSON.stringify({
                            type: "clarify_response",
                            request_id: requestId,
                            answer: "",
                          }),
                        );
                      } catch {}
                    });
                }
                return;
              }

              // M5: ACUI directive (agent → UI structured card push).
              if (evtType === "acui" || evtType === "Acui") {
                const directive = data?.directive ?? data?.event?.directive;
                if (directive && typeof directive === "object" && "op" in directive) {
                  callbacks.onAcuiDirective?.(directive as AcuiDirective);
                }
                return;
              }

// Context usage snapshot — fired after every agent-loop turn.
// Used to render a persistent context-usage bar in the chat footer.
              if (evtType === "context_usage" || evtType === "ContextUsage") {
                // Drop events whose WS session != the active session, so a
                // background session's context_usage can't pollute the bar of
                // the session the user switched to.
                if (activeSessionIdRef.current && wsSessionIdRef.current
                    && activeSessionIdRef.current !== wsSessionIdRef.current) {
                  return;
                }
                const used = typeof (data?.used_tokens ?? data?.event?.used_tokens) === "number"
                  ? (data?.used_tokens ?? data?.event?.used_tokens) as number
                  : 0;
                const max = typeof (data?.max_tokens ?? data?.event?.max_tokens) === "number"
                  ? (data?.max_tokens ?? data?.event?.max_tokens) as number
                  : 0;
                const threshold = typeof (data?.threshold_tokens ?? data?.event?.threshold_tokens) === "number"
                  ? (data?.threshold_tokens ?? data?.event?.threshold_tokens) as number
                  : 0;
                const pct = typeof (data?.percent ?? data?.event?.percent) === "number"
                  ? (data?.percent ?? data?.event?.percent) as number
                  : 0;
                setContextUsage({ usedTokens: used, maxTokens: max, thresholdTokens: threshold, percent: pct });
                return;
              }

              // per-turn usage snapshot — backend fires
              // this after every successful LLM response so the chat
              // header can show a live "cache hit %" chip next to
              // the context-usage bar. Mirrors `context_usage`
              // shape (top-level OR nested under `event`). We keep
              // the raw counts; the chip computes its own rate so
              // the formula stays in one place (the consumer).
              // Wire shape:
              // { "type": "usage_update",
              // "event": { prompt_tokens, completion_tokens,
              // cache_read_tokens, cache_creation_tokens,
              // reasoning_tokens } }
              if (evtType === "usage_update" || evtType === "UsageUpdate") {
                // same session filter as context_usage above.
                if (activeSessionIdRef.current && wsSessionIdRef.current
                    && activeSessionIdRef.current !== wsSessionIdRef.current) {
                  return;
                }
                const ev = data?.event ?? data ?? {};
                const toNum = (v: unknown): number =>
                  typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : 0;
                setUsageSnapshot({
                  promptTokens: toNum(ev.prompt_tokens),
                  completionTokens: toNum(ev.completion_tokens),
                  cacheReadTokens: toNum(ev.cache_read_tokens),
                  cacheCreationTokens: toNum(ev.cache_creation_tokens),
                  reasoningTokens: toNum(ev.reasoning_tokens),
                });
                return;
              }

              // compaction-pipeline status snapshot
              // forwarded from the backend's `BreakerSnapshot`. The
              // server emits it both on pressure-detected and after
              // every compression attempt; the `CompactionStatusBanner`
              // derives its 5-state UI from `state` and renders the
              // "145k / 160k tokens (90%)" footer from
              // `used_tokens` / `threshold_tokens`.
              // Wire shape (top-level OR nested under `event`):
              // { "event": "compaction_status", "snapshot": { state, ... } }
              if (evtType === "compaction_status" || evtType === "CompactionStatus") {
                const snapRaw =
                  data?.snapshot ?? data?.event?.snapshot ?? null;
                if (snapRaw && typeof snapRaw === "object") {
                  const snap = snapRaw as Record<string, unknown>;
                  const rawState = snap.state;
                  const state: import("../components/CompactionStatusBanner").CompactionState =
                    rawState === "open" ||
                    rawState === "half_open" ||
                    rawState === "closed" ||
                    rawState === "fallback" ||
                    rawState === "pressure" ||
                    rawState === "quota_exhausted"
                      ? (rawState as import("../components/CompactionStatusBanner").CompactionState)
                      : "closed";
                  const toNumber = (v: unknown): number | undefined =>
                    typeof v === "number" && Number.isFinite(v) ? v : undefined;
                  const usedTokens = toNumber(snap.used_tokens);
                  const thresholdTokens = toNumber(snap.threshold_tokens);
                  const maxTokens = toNumber(snap.max_tokens);
                  // Mirror the server's `format_token_count` so the
                  // banner reads "145k / 160k" (the same string the
                  // status chip in StreamingBubble would render).
                  const fmt = (n: number): string => {
                    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
                    if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
                    return String(n);
                  };
                  const unit =
                    typeof snap.unit === "string" && snap.unit.length > 0
                      ? (snap.unit as string)
                      : "tokens";
                  // Use the same denominator as the ChatView progress bar
                  // (max context window) so banner and bar never disagree.
                  const percent =
                    usedTokens !== undefined && maxTokens && maxTokens > 0
                      ? Math.min(100, (usedTokens / maxTokens) * 100)
                      : usedTokens !== undefined && thresholdTokens && thresholdTokens > 0
                        ? Math.min(100, (usedTokens / thresholdTokens) * 100)
                        : undefined;
                  const lastFailureReason =
                    typeof snap.last_failure_reason === "string"
                      ? (snap.last_failure_reason as string)
                      : undefined;
                  const lastChangedAt =
                    typeof snap.last_changed_at_unix_ms === "number"
                      ? (snap.last_changed_at_unix_ms as number)
                      : undefined;
                  // forward the compression-process phase
                  // so `CompactionStatusBanner` can render the
                  // "starting / in_progress / complete / failed"
                  // transitions instead of jumping straight from the
                  // pressure warning to the post-attempt result.
                  // Older snapshots without the field decode as
                  // `"idle"` (additive change, no wire break).
                  const rawPhase = typeof snap.phase === "string" ? snap.phase : "idle";
                  const phase: import("../components/CompactionStatusBanner").CompactionPhase =
                    rawPhase === "starting" ||
                    rawPhase === "in_progress_fast" ||
                    rawPhase === "in_progress_llm" ||
                    rawPhase === "complete" ||
                    rawPhase === "failed"
                      ? rawPhase
                      : "idle";
                  setCompactionStatus({
                    state,
                    usedText: usedTokens !== undefined ? fmt(usedTokens) : undefined,
                    thresholdText: thresholdTokens !== undefined ? fmt(thresholdTokens) : undefined,
                    maxText: maxTokens !== undefined ? fmt(maxTokens) : undefined,
                    usedTokens,
                    thresholdTokens,
                    maxTokens,
                    unit,
                    percent,
                    consecutiveFailures:
                      toNumber(snap.consecutive_llm_failures) ?? undefined,
                    lastFailureReason,
                    lastChangedAt,
                    phase,
                  });
                }
                return;
              }

              // (C3 marker protocol): the backend's
              // `SectionGate` runs on every `resolve_initial_system_prompt`
              // and forwards the resulting `SectionGateReport` to the
              // `on_section_gate` callback, which serialises it as a
              // `section_gate` WS event. We append it to the
              // execution_timeline stream so the trace panel can
              // render "SectionGate patched 2 required sections:
              // Working directory, Current time" inline with the
              // tool cards the user already sees.
              // Wire shape (top-level OR nested under `event`):
              // {
              // "event": "section_gate",
              // "report": {
              // "patched": [{ "section": "working_dir", "applied": true, "reason": "..." }],
              // "pending": [{ "section": "tool_list", "applied": false, "reason": "..." }]
              // }
              // }
              if (evtType === "section_gate" || evtType === "SectionGate") {
                const reportRaw =
                  data?.report ?? data?.event?.report ?? null;
                if (reportRaw && typeof reportRaw === "object") {
                  const report = reportRaw as {
                    patched?: Array<{
                      section: string;
                      applied?: boolean;
                      reason?: string;
                    }>;
                    pending?: Array<{
                      section: string;
                      applied?: boolean;
                      reason?: string;
                    }>;
                  };
                  callbacks.onSectionGate?.({
                    patched: Array.isArray(report.patched)
                      ? report.patched.map((g) => ({
                          section:
                            (g?.section as
                              | "identity"
                              | "tool_list"
                              | "tool_enforce"
                              | "working_dir"
                              | "timestamp") ?? "identity",
                          applied: !!g?.applied,
                          reason: typeof g?.reason === "string" ? g.reason : "",
                        }))
                      : [],
                    pending: Array.isArray(report.pending)
                      ? report.pending.map((g) => ({
                          section:
                            (g?.section as
                              | "identity"
                              | "tool_list"
                              | "tool_enforce"
                              | "working_dir"
                              | "timestamp") ?? "identity",
                          applied: !!g?.applied,
                          reason: typeof g?.reason === "string" ? g.reason : "",
                        }))
                      : [],
                  });
                }
                return;
              }

              if (evtType === "done") {
                const reason: string | undefined =
                  data?.finish_reason || data?.event?.finish_reason;
                // Terminal outcome classification from the server.
                // `completed` = turn finished naturally, safe to close
                // the WS. `failed` / `interrupted` = the agent loop ended
                // without a usable result (backend error, invalid-tool
                // exhaustion, user stop). We MUST keep the session alive
                // and surface a recoverable prompt — a backend failure is
                // not a session death (a reload would otherwise lose the
                // partial work and the pending approval). Backward-compat:
                // older servers omit `outcome`, so treat absence as
                // "completed" only when finish_reason isn't a known
                // failure signal.
                const outcome: string | undefined =
                  data?.outcome || data?.event?.outcome;
                const isFailedOutcome =
                  outcome === "failed" || outcome === "interrupted";
                // (defensive) a backend-caused failure may arrive as a
                // `done` with finish_reason "error" (e.g. a future
                // refactor or an older server build). Route it to the
                // error path so the user sees a red banner + "出错了"
                // instead of a misleading "completed" with partial content.
                // `done.error` carries the raw backend error (LLM 401/402/429,
                // 5xx, transport error) — mirrors `AgentResult.last_error`.
                // Render it whenever present, independent of the
                // `outcome`/`finish_reason` sentinels, so a backend-flagged
                // failure is never silently collapsed into "completed".
                const rawError: string | undefined =
                  data?.error || data?.event?.error;
                if (isFailedOutcome || reason === "error" || !!rawError) {
                  // Backend failure / unfinished turn: keep the session
                  // alive (don't close the WS) so the user can retry or a
                  // pending approval can still resolve. Surface a red
                  // banner + "出错了" instead of a misleading "completed".
                  //
                  // If the backend supplied a raw error (LLM 401/402/429,
                  // 5xx body, transport error, etc.) it's now carried on
                  // `event.error` (mirrors `AgentResult.last_error`); the
                  // `done.error` field replaced the old "技术详情 blind"
                  // behaviour where the user only saw a generic banner.
                  // When `rawError` is present we pass it as the primary
                  // message so `localizeStreamError` in App.tsx can classify
                  // it (e.g. 529 overloaded -> "AI 服务过载..."); fall back
                  // to a generic prompt only when the server omitted details.
                  const errMsg = rawError ?? "对话因后端错误中断（已保留已生成的部分内容，请稍后重试）";
                  // Surface the backend's coarse classification as `kind`
                  // when it shipped one (`provider_overloaded`,
                  // `quota_exhausted`, …). Falls back to `agent_error` so
                  // the UI always has a category to render instead of
                  // "对话因后端错误中断".
                  const errKind =
                    (typeof data?.code === "string" && data.code) ||
                    (typeof data?.event?.code === "string" && data.event.code) ||
                    (rawError ? "agent_error" : undefined);
                  settle("", errMsg, undefined, "error", rawError, true, errKind);
                  return;
                }
                // M3 may send a `done` with empty `evtContent` (the
                // finish_reason frame doesn't contain the streamed
                // text). Always prefer `fullText` (the accumulated
                // text from all `text` events) over `evtContent`.
                // Only use `evtContent` if it's non-empty — it might
                // contain additional info (like tool XML) that should
                // be treated as text, but empty is never correct.
                const text = (typeof evtContent === "string" && evtContent.length > 0)
                  ? evtContent
                  : fullText;
                // a `done` whose finish_reason is one of
                // the watchdog / timeout signals is *not* a clean
                // completion. The server gave up because nothing
                // useful happened (e.g. `turn_transition_stall`,
                // `no_first_chunk`, `stream_overall_timeout`). Map
                // these to the existing `timeout` phase so the user
                // sees "对话超时" / a retry button instead of the
                // misleading "对话已完成" with empty assistant text.
                const isWatchdogOrTimeout =
                  reason === "turn_transition_stall" ||
                  reason === "no_first_chunk" ||
                  reason === "stream_overall_timeout";
                const phase: StreamPhase = isWatchdogOrTimeout
                  ? "timeout"
                  : "completed";
                settle(
                  text,
                  undefined,
                  reason,
                  phase,
                );
                return;
              }

              if (evtType === "error") {
                const errMsg = data?.message || data?.event?.message || "Stream error";
                const errDetail = data?.detail || data?.event?.detail;
                const errKind = data?.code || data?.event?.code;
                settle("", errMsg, undefined, "error", errDetail, undefined, errKind);
                return;
              }

              // H3: prefetch intent event — prefetcher resolved with a
              // user-intent prediction and speculatively ran safe tools.
              if (evtType === "prefetch_intent") {
                const ev = data?.event ?? data;
                const intent = ev?.intent ?? "unknown";
                const confidence = ev?.confidence ?? 0;
                const results: PrefetchResultEntry[] = Array.isArray(ev?.results)
                  ? ev.results.map((r: any) => ({
                      tool: r?.tool ?? "unknown",
                      args: r?.args ?? {},
                      output: r?.output ?? "",
                      errored: !!r?.errored,
                      durationMs: r?.duration_ms ?? r?.durationMs ?? 0,
                    }))
                  : [];
              onPrefetchRef.current?.({ intent, confidence, results });
                return;
              }
            } catch {
              // ignore malformed frames
            }
          };

          ws.onerror = () => {
            // Stale WS guard.
            if (activeSendIdRef.current !== mySendId) return;
            if (!doneRef.current) {
              // Once we've received any content (text, thinking, tools,
              // file/memory/todo/sub-agent events), we MUST NOT retry — the
              // agent loop is already running server-side and a fresh WS would
              // trigger a second `send_message_stream` call for the same content.
              // Settle "partial" so the user keeps what already arrived instead
              // of discarding it via an error settle.
              if (hasAnyStreamEvent || hasContent) {
                settle(fullText, undefined, undefined, "partial");
                return;
              }
              // if the message was already sent to the
              // server (we received `connected` and called
              // `ws.send(payload)`), DO NOT retry — the server is
              // already processing it. A new WS would re-send the
              // same text, creating a second agent loop that
              // produces duplicate assistant messages in the DB.
              if (sentToServerRef.current) {
                settle("", "Connection lost while server was processing the message", undefined, "error");
                return;
              }
              if (retryCount < MAX_RETRIES) {
                retryCount++;
                const delay = RETRY_DELAYS_MS[Math.min(retryCount - 1, RETRY_DELAYS_MS.length - 1)];
                const __sendDiagId3 = nextDiagId();
                diagLog(`[diag #${__sendDiagId3}] onerror → retry ${retryCount}/${MAX_RETRIES}, delay=${delay}ms`);
                updatePhase("reconnecting");
                wsRef.current = null;
                setTimeout(doConnect, delay);
                return;
              }
              settle("", "WebSocket error", undefined, "error");
            }
          };
          ws.onclose = () => {
            // Stale WS guard — this is the critical fix.
            // When settle() closes the WS, the `onclose` event is
            // queued as a macro task. By the time it fires, a new
            // `send()` may have already started (resetting doneRef
            // and swapping wsRef). Without this guard, the stale
            // `onclose` would call settle() again, closing the NEW
            // WebSocket and causing the new message to show
            // "disconnected".
            if (activeSendIdRef.current !== mySendId) return;
            if (!doneRef.current) {
              if (hasAnyStreamEvent) {
                // Any non-status event arrived (text, thinking, tools,
                // file/memory/todo/sub-agent) — the agent loop was running,
                // so preserve whatever we have as a partial result instead of
                // treating it as a connection error that wipes the streaming
                // state and misleads the user with "可以继续提问".
                settle(fullText, undefined, undefined, "partial");
              } else if (hasContent) {
                // Fallback for text-only streams (text events set both flags).
                settle(fullText, undefined, undefined, "partial");
              } else {
                // same hardening as onerror — if the
                // message was already sent, don't retry.
                if (sentToServerRef.current) {
                  settle("", "Connection closed while server was processing the message", undefined, "error");
                  return;
                }
                if (retryCount < MAX_RETRIES) {
                  retryCount++;
                  const delay = RETRY_DELAYS_MS[Math.min(retryCount - 1, RETRY_DELAYS_MS.length - 1)];
                  const __sendDiagId4 = nextDiagId();
                  diagLog(`[diag #${__sendDiagId4}] onclose (no content) → retry ${retryCount}/${MAX_RETRIES}, delay=${delay}ms`);
                  updatePhase("reconnecting");
                  wsRef.current = null;
                  setTimeout(doConnect, delay);
                  return;
                }
                settle("", "Connection closed", undefined, "error");
              }
            }
            // Note: stale approval resolvers are cleared by App.tsx
            // when `streamingSessionId` flips to null (see
            // `useEffect` cleanup that drains the
            // `approvalResolversRef` Map). We deliberately do not
            // re-implement that cleanup here — `useStreamChat`
            // doesn't own those resolvers, App.tsx does.
          };
        } catch (e) {
          // WS constructor threw (unlikely — URL parsing error, etc.)
          // Try once more before settling.
          if (!hasAnyStreamEvent && !hasContent && retryCount < MAX_RETRIES) {
            retryCount++;
            const delay = RETRY_DELAYS_MS[Math.min(retryCount - 1, RETRY_DELAYS_MS.length - 1)];
            updatePhase("reconnecting");
            wsRef.current = null;
            setTimeout(doConnect, delay);
            return;
          }
          settle("", e instanceof Error ? e.message : String(e));
        }
        }; // end doConnect

        // Start the first connection attempt.
        doConnect();
      });
    },
    []
  );

  const cancel = useCallback(() => {
    // If there's an active send, settle it immediately so the
    // UI's `sending` state gets reset without waiting for the
    // asynchronous WS `onclose` event.  `settle()` is a no-op
    // if it already ran (`done` guard), so double-cancel is safe.
    if (settleRef.current) {
      // 在 settle 关闭 WS **之前**通知调用方：此时 WS 仍 OPEN，可把
      // 仍在途的审批/澄清直接否决发往后端，使 agent loop 立即解除阻塞。
      pendingCallbacksRef.current?.onCancel?.();
      // Disarm timeouts first so they don't fire after we settle.
      disarmTimeoutsRef.current?.();
      settleRef.current("", undefined, "stop", "completed");
    }
    // Defensive: ensure WS is closed and streaming state is cleared
    // even if settle() somehow didn't run (e.g. settleRef was
    // already nulled by a racing onclose handler).
    console.log("[WS-CLOSE] site=cancel (用户点击停止的防御性兜底)");
    wsRef.current?.close();
    wsRef.current = null;
    streamingSessionIdRef.current = null;
    setStreamingSessionId(null);
  }, []);

  return { send, streaming: streamingSessionId !== null, streamingSessionId, streamingStatus, lastActivityAt, compactionStatus, setCompactionStatus, contextUsage, setContextUsage, usageSnapshot, setUsageSnapshot, cancel, wsSendRaw };
}

export type StreamChatApi = ReturnType<typeof useStreamChat>;
