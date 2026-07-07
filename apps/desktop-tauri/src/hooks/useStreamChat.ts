import { useState, useCallback, useRef, useEffect } from "react";
import { resolveWebSocketUrl } from "../api";
import type { AcuiDirective } from "../acui/types";

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
}

/**
 * Lifecycle phase of a single chat send. Returned by `useStreamChat`
 * so the caller can drive the UI through every stage without
 * collapsing them all into a boolean "isStreaming" — the old
 * boolean was the root cause of the "已中断" + full reply text
 * appearing simultaneously bug.
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
 * • `timeout`  — 120s elapsed with no `done`; the stream is
 *                genuinely abandoned and the chip is "AI 响应超时"
 *                in warning tone.
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
  /** 2026-06-26: 模型路由 — 形如 "minimax:MiniMax-M2.7" 的 "provider:model" 串。
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
  /** 2026-06-25: 多图支持 — binary bytes 直传，不走 base64。 */
  images?: { bytes: number[]; mimeType: string; name?: string }[];
  image?: string;  // legacy single-image base64 (backward compat)
  imageMimeType?: string;  // mime type for legacy single image
  /** 非图片本地文件路径（拖放/粘贴时获取），后端直接用 read_file/read_pdf 读取 */
  attachedFilePaths?: string[];
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
  onToolComplete?: (info: { toolCallId: string; tool: string; content?: string }) => void;
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
   * A todo list mutation fired by the server (always a TodoItemUpserted
   * or TodoItemRemoved — TodoSnapshot is sent only on (re)connect in
   * the future). The TodoPanel hooks into this to render the live
   * task list.
   *
   * Mirrors `StreamEvent::TodoItemUpserted` / `StreamEvent::TodoItemRemoved`
   * in hermix-transport. `sessionId` is stamped by the server based
   * on which WS connection produced the change.
   */
  onTodoChange?: (change:
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
     *  `undefined` for pre-2026-06-14 servers that predate the
     *  countdown feature — the card falls back to a no-countdown
     *  state in that case. */
    expiresAt?: number | null;
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
  onDone?: (fullText: string, stopReason?: string) => void;
  onError?: (error: string) => void;
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
   * 2026-06-29 (C3 marker protocol): the backend's `SectionGate`
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
}

export function useStreamChat() {
  // Per-session streaming state. We track the *session id* (not just a
  // boolean) so the UI can scope the "正在思考…" indicator and the
  // composer-disabled state to the specific session that's mid-stream.
  //
  // A global `boolean` was the previous design and it had two
  // pathologies:
  //   1. Switching to another session mid-stream would leak the
  //      "thinking" state into the new session's ChatView (false
  //      positive — the new session isn't actually streaming).
  //   2. After `cancel()` closed the active WS, React batching of the
  //      state update could let the Composer stay disabled across
  //      session switches, blocking new sends until a full
  //      re-render cycle.
  // The id-typed state fixes both: ChatView only shows the indicator
  // when `streamingSessionId === activeSessionId`, and `null` is the
  // unambiguous "no stream in flight" sentinel.
  const [streamingSessionId, setStreamingSessionId] = useState<string | null>(null);
  // Transient status text from the backend (e.g. "Still waiting for the model…").
  const [streamingStatus, setStreamingStatus] = useState<string | null>(null);
  // Compaction pipeline status (Day 1-B placeholder). In Day 2 the backend
  // will emit a separate `compaction_status` WS event kind and we'll
  // parse it here. For now it's just a state slot wired through so
  // ChatView can render the banner without further plumbing work later.
  const [compactionStatus, setCompactionStatus] =
    useState<import("../components/CompactionStatusBanner").CompactionStatus | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  // Refs to hold the per-send promise resolvers + callbacks,
  // so cancel() can settle the promise immediately without waiting
  // for the async WS onclose event.
  const pendingResolveRef = useRef<((value: string) => void) | null>(null);
  const pendingRejectRef = useRef<((error: Error) => void) | null>(null);
  const pendingCallbacksRef = useRef<StreamCallbacks | null>(null);
  const disarmTimeoutsRef = useRef<(() => void) | null>(null);
  // Tracks whether the current send's promise has already been settled.
  // We use a ref (not a local var) so cancel() can read it across
  // closure boundaries without needing access to `send`'s internals.
  const sendSettledRef = useRef(false);
  // Holds the `settle` function of the current `send` call so
  // `cancel()` can invoke it directly (synchronously resolving the
  // promise) instead of waiting for the async WS `onclose` event.
  const settleRef = useRef<((text: string, err?: string, reason?: string, phaseHint?: StreamPhase) => void) | null>(null);
  // Client-side dedup: track last sent (hash, timestamp) to prevent
  // the same content from being sent multiple times within the window.
  const lastSendRef = useRef<{ hash: string; ts: number } | null>(null);
  // 2026-07-07: shared per-send state via refs so that WS reconnection
  // (doConnect retry) shares the same `done` / `sentToServer` flags
  // instead of creating fresh local variables each time.
  const doneRef = useRef(false);
  const sentToServerRef = useRef(false);
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
        // Expose resolvers + callbacks so cancel() can settle the
        // promise immediately without waiting for the async WS onclose.
        pendingResolveRef.current = resolve;
        pendingRejectRef.current = reject;
        pendingCallbacksRef.current = callbacks;
        sendSettledRef.current = false;
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
        //
        // Collapse N chunks into one batch per rAF tick (≤16ms on a
        // 60Hz display). The "正在打字…" cursor still animates every
        // frame; we just stop forcing a full React reconciliation on
        // every token. The on-hook `fullText` accumulator is still
        // updated synchronously so `onDone` (settle) gets the full
        // server text regardless of when rAF last fired.
        //
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

        // ── Client-side message dedup ─────────────────────────────
        // 2026-07-07: prevent the same content from being sent
        // multiple times within DEDUP_WINDOW_MS. This catches:
        //   - Multiple doConnect calls racing (React re-render)
        //   - WS reconnect after agent already completed
        //   - handleSubmit called twice rapidly
        const DEDUP_WINDOW_MS = 15_000;
        const contentHash = `${sessionId}:${content}`;
        const now = Date.now();
        if (
          lastSendRef.current &&
          lastSendRef.current.hash === contentHash &&
          now - lastSendRef.current.ts < DEDUP_WINDOW_MS
        ) {
          console.warn(
            `[useStreamChat] dedup: same content sent ${now - lastSendRef.current.ts}ms ago, skipping`
          );
          return;
        }
        lastSendRef.current = { hash: contentHash, ts: now };

        // ── Per-send retry state ─────────────────────────────────
        // 2026-07-06: auto-retry when the WS fails before any content
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
        // 2026-07-07: true once `ws.send(payload)` succeeds in the
        // `connected` handler. If the WS closes before any content
        // arrives but AFTER the message was sent to the server, we
        // MUST NOT retry — the server already received it and will
        // eventually respond (or complete). Retrying with a fresh WS
        // sends the same content again, creating a second agent loop
        // and two assistant replies in the DB — visible to the user
        // as "重复回复".
        sentToServerRef.current = false;
        let totalTimeout: ReturnType<typeof setTimeout> | null = null;
        // 30s first-chunk slow-hint timer handle (see ws.onmessage "connected"
        // branch). Tracked so settle()/disarmTimeouts() can cancel it — without
        // a handle, a stale 30s timer can fire after the stream already
        // terminated, flipping `phase` back to "slow" right after the user
        // already saw "completed".
        let slowHintTimer: ReturnType<typeof setTimeout> | null = null;
        const TOTAL_TIMEOUT_MS = 120_000;

        const armTimeouts = () => {
          if (totalTimeout !== null) clearTimeout(totalTimeout);
          totalTimeout = setTimeout(() => {
            if (!doneRef.current) {
              updatePhase("timeout");
              // Force-settle so the promise doesn't hang forever.
              // Without this the UI stays stuck in "timeout" phase
              // with `sending=true` indefinitely.
              settleRef.current?.("", undefined, "timeout", "timeout");
            }
          }, TOTAL_TIMEOUT_MS);
        };
        const disarmTimeouts = () => {
          if (totalTimeout !== null) {
            clearTimeout(totalTimeout);
            totalTimeout = null;
          }
          if (slowHintTimer !== null) {
            clearTimeout(slowHintTimer);
            slowHintTimer = null;
          }
        };
        disarmTimeoutsRef.current = disarmTimeouts;

        const settle = (text: string, err?: string, reason?: string, phaseHint?: StreamPhase) => {
          if (doneRef.current) return;
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
          setStreamingSessionId(null);
          setStreamingStatus(null);
          wsRef.current?.close();
          wsRef.current = null;
          if (reason) stopReason = reason;
          // Emit terminal phase
          updatePhase(phaseHint ?? (err ? "error" : "completed"));

          // Strip leaked status/activity markers from final text
          // (defense-in-depth: old backend may have injected these
          // as Text events before the lib.rs fix was compiled).
          const cleanText = text.replace(/\n?\[(?:activity|status):[^\]]*\]\s*/g, "");

          if (err) {
            callbacks.onError?.(err);
            reject(new Error(err));
          } else {
            callbacks.onDone?.(cleanText, stopReason);
            resolve(cleanText);
          }
          // Clean up per-send refs so a stale cancel() call
          // can't settle an already-settled promise.
          pendingResolveRef.current = null;
          pendingRejectRef.current = null;
          pendingCallbacksRef.current = null;
          sendSettledRef.current = true;
          settleRef.current = null;
        };
        settleRef.current = settle;

        // 2026-07-06: extracted WS creation into a callable so the
        // auto-retry path (before any content arrives) can create a
        // fresh WS without duplicating the ~200 lines of handler setup.
        const doConnect = () => {
        try {
          const ws = new WebSocket(resolveWebSocketUrl(sessionId));
          wsRef.current = ws;

          // Approval responses are routed back to the original
          // `callbacks.onApprovalRequest(...)` Promise through a
          // closure (the request id flows in the WS frame and the
          // corresponding resolver lives in `App.tsx` via
          // `approvalResolversRef`). The previous implementation
          // kept a local `pendingApprovals` Map here that was
          // populated by nothing and only read on `onclose` —
          // dead code that hid the real lifecycle (cleanup of
          // stale resolvers happens in App.tsx when a stream
          // settles or the session changes).

          ws.onmessage = (evt) => {
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
                // 2026-06-26: 把 selectedModel（形如 "minimax:MiniMax-M2.7"）
                // 透传到 backend，让 effective_model 真的能切到用户选的 provider。
                // 没设 = backend 用 config.yaml 默认 model。bug 历史：UI 显示切了
                // 但 backend 永远在跑默认模型。
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
                firstChunkReceived = true;
                if (phase === "slow") updatePhase("streaming");
                const piece = typeof evtContent === "string" ? evtContent : "";
                // Defense-in-depth: if the old backend still injects
                // [activity: …] / [status: …] as Text events (before
                // the lib.rs fix was compiled), silently drop them so
                // they never pollute message body.
                if (piece && !/^\[(?:activity|status):\s*/.test(piece)) {
                  hasContent = true;
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
                  callbacks.onReasoningChunk?.(piece);
                }
                return;
              }

              if (evtType === "tool_start") {
                const tool = data?.tool || data?.event?.tool || "tool";
                const toolCallId =
                  data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
                callbacks.onToolStart?.({ toolCallId, tool, content: evtContent });
                return;
              }

              if (evtType === "tool_complete") {
                const tool = data?.tool || data?.event?.tool || "tool";
                const toolCallId =
                  data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
                callbacks.onToolComplete?.({ toolCallId, tool, content: evtContent });
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

              // Todo (task list) mutations. Wire format:
              //   { type: "todo_item_upserted", event: { session_id, item: {...} } }
              //   { type: "todo_item_removed",  event: { session_id, id: "..." } }
              // Routing is straight to onTodoChange; no message-bubble
              // side effects (the TodoPanel subscribes directly).
              if (evtType === "todo_item_upserted") {
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
                const ev = data?.event ?? data;
                const requestId = ev?.request_id ?? ev?.requestId;
                const tool = ev?.tool ?? "tool";
                const args = ev?.args ?? ev?.arguments;
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
                    .onApprovalRequest({ requestId, tool, args, riskLevel, expiresAt, sessionId })
                    .then((resp) => {
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
                      if (resp.decision === "expired") return;
                      try {
                        // Server expects "approved" or "denied" (exact match
                        // against `decision == "approved"` in lib.rs:1225).
                        const decision = resp.decision === "approved" ? "approved" : "denied";
                        ws.send(
                          JSON.stringify({
                            type: "approval_response",
                            request_id: requestId,
                            decision,
                          }),
                        );
                      } catch {
                        // socket already closed
                      }
                    })
                    .catch(() => {
                      try {
                        ws.send(
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
                      try {
                        ws.send(
                          JSON.stringify({
                            type: "clarify_response",
                            request_id: requestId,
                            answer: resp.answer ?? "",
                          }),
                        );
                      } catch {
                        // socket already closed
                      }
                    })
                    .catch(() => {
                      try {
                        ws.send(
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

              // 2026-06-29: compaction-pipeline status snapshot
              // forwarded from the backend's `BreakerSnapshot`. The
              // server emits it both on pressure-detected and after
              // every compression attempt; the `CompactionStatusBanner`
              // derives its 5-state UI from `state` and renders the
              // "145k / 160k tokens (90%)" footer from
              // `used_tokens` / `threshold_tokens`.
              //
              // Wire shape (top-level OR nested under `event`):
              //   { "event": "compaction_status", "snapshot": { state, ... } }
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
                    rawState === "pressure"
                      ? (rawState as import("../components/CompactionStatusBanner").CompactionState)
                      : "closed";
                  const toNumber = (v: unknown): number | undefined =>
                    typeof v === "number" && Number.isFinite(v) ? v : undefined;
                  const usedTokens = toNumber(snap.used_tokens);
                  const thresholdTokens = toNumber(snap.threshold_tokens);
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
                  const percent =
                    usedTokens !== undefined && thresholdTokens && thresholdTokens > 0
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
                  setCompactionStatus({
                    state,
                    usedText: usedTokens !== undefined ? fmt(usedTokens) : undefined,
                    thresholdText: thresholdTokens !== undefined ? fmt(thresholdTokens) : undefined,
                    unit,
                    percent,
                    consecutiveFailures:
                      toNumber(snap.consecutive_llm_failures) ?? undefined,
                    lastFailureReason,
                    lastChangedAt,
                  });
                }
                return;
              }

              // 2026-06-29 (C3 marker protocol): the backend's
              // `SectionGate` runs on every `resolve_initial_system_prompt`
              // and forwards the resulting `SectionGateReport` to the
              // `on_section_gate` callback, which serialises it as a
              // `section_gate` WS event. We append it to the
              // execution_timeline stream so the trace panel can
              // render "SectionGate patched 2 required sections:
              // Working directory, Current time" inline with the
              // tool cards the user already sees.
              //
              // Wire shape (top-level OR nested under `event`):
              //   {
              //     "event": "section_gate",
              //     "report": {
              //       "patched": [{ "section": "working_dir", "applied": true, "reason": "..." }],
              //       "pending": [{ "section": "tool_list", "applied": false, "reason": "..." }]
              //     }
              //   }
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
                settle(
                  text,
                  undefined,
                  reason,
                  "completed",
                );
                return;
              }

              if (evtType === "error") {
                settle("", data?.message || data?.event?.message || "Stream error", undefined, "error");
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
            if (!doneRef.current) {
              // 2026-07-06 (rev): if we've ever received any content
              // (text or status), we MUST NOT retry — the agent
              // loop is already running server-side and a fresh WS
              // would trigger a second `send_message_stream` call
              // for the same content. We settle "partial" so the
              // user keeps the text we've already received instead
              // of discarding it via an error settle.
              if (hasContent) {
                settle(fullText, undefined, undefined, "partial");
                return;
              }
              // 2026-07-07: if the message was already sent to the
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
                updatePhase("reconnecting");
                wsRef.current = null;
                setTimeout(doConnect, delay);
                return;
              }
              settle("", "WebSocket error", undefined, "error");
            }
          };
          ws.onclose = () => {
            if (!doneRef.current) {
              if (hasContent && fullText.length > 0) {
                settle(fullText, undefined, undefined, "partial");
              } else {
                // 2026-07-06 (rev): same hardening as onerror above.
                // If anything arrived (status, thinking, anything),
                // never retry — the agent loop is already running.
                if (hasContent) {
                  settle(fullText, undefined, undefined, "partial");
                  return;
                }
                // 2026-07-07: same hardening as onerror — if the
                // message was already sent, don't retry.
                if (sentToServerRef.current) {
                  settle("", "Connection closed while server was processing the message", undefined, "error");
                  return;
                }
                if (retryCount < MAX_RETRIES) {
                  retryCount++;
                  const delay = RETRY_DELAYS_MS[Math.min(retryCount - 1, RETRY_DELAYS_MS.length - 1)];
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
          if (!hasContent && retryCount < MAX_RETRIES) {
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
      // Disarm timeouts first so they don't fire after we settle.
      disarmTimeoutsRef.current?.();
      settleRef.current("", undefined, "stop", "completed");
    }
    // Defensive: ensure WS is closed and streaming state is cleared
    // even if settle() somehow didn't run (e.g. settleRef was
    // already nulled by a racing onclose handler).
    wsRef.current?.close();
    wsRef.current = null;
    setStreamingSessionId(null);
  }, []);

  return { send, streaming: streamingSessionId !== null, streamingSessionId, streamingStatus, compactionStatus, setCompactionStatus, cancel };
}
