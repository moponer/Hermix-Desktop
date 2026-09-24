import type { MutableRefObject, Dispatch, SetStateAction } from "react";
import { invoke } from "@tauri-apps/api/core";
import { streamChannelName } from "../api";
import type { AcuiDirective } from "../acui/types";
import type { CompactionStatus } from "../components/CompactionStatusBanner";
import type {
  StreamPhase,
  ChatOverrides,
  StreamCallbacks,
  PrefetchResultEntry,
} from "./useStreamChat";
import { narrowSectionGateReport } from "./streamChatUtils";

/** Context-usage snapshot pushed by the backend each turn. */
export interface ContextUsage {
  usedTokens: number;
  maxTokens: number;
  thresholdTokens: number;
  percent: number;
}
/** Per-turn token usage snapshot (cache hit % chip). */
export interface UsageSnapshot {
  promptTokens: number;
  completionTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  reasoningTokens: number;
}

/**
 * Everything the `ws.onmessage` / `ws.onerror` / `ws.onclose` handlers
 * need from the `useStreamChat` `send` closure. The original code kept
 * all of this as bare `let`s and refs inside `send`; we hoist them onto
 * a single mutable object so the parsing + connection logic can live in
 * their own modules without rewriting the closure shape.
 *
 * Per-send mutable locals (fullText / phase / hasContent / …) are plain
 * fields on this object and are mutated in place by the handlers.
 */
export interface StreamChatSendContext {
  sessionId: string;
  content: string;
  overrides: ChatOverrides;
  callbacks: StreamCallbacks;
  mySendId: number;

  // ── component-level refs (stable across sends) ──
  activeSendIdRef: MutableRefObject<number>;
  // Holds the stdio-bridge listener unlisten function (was
  // `WebSocket | null` in the TCP era). The connector assigns
  // `listenStreamChannel(...)`'s returned fn here on connect and
  // calls it to tear down.
  wsRef: MutableRefObject<(() => void) | null>;
  // Tool callback for "WS reconnection exhausted and no events have
  // been received yet" — fired by the connector ONLY when there is
  // genuinely nothing else to fall back to (i.e. message was sent to
  // the server). The caller is responsible for showing an
  // immediately-visible "网络中断" toast and starting any fallback
  // (REST polling, retry, etc.) so the user is not stuck on a stale
  // "正在回复…" spinner for minutes.
  onConnectionLost?: () => void;
  // Tool callback for "WebSocket is back online after a retry attempt"
  // — fires when the live WS transitions to OPEN after a previously
  // fired `onConnectionLost`. Lets the caller clear the "网络中断"
  // toast / banner once streaming resumes.
  onConnectionRestored?: () => void;
  wsSessionIdRef: MutableRefObject<string | null>;
  doneRef: MutableRefObject<boolean>;
  sentToServerRef: MutableRefObject<boolean>;
  toolRunningRef: MutableRefObject<boolean>;
  activeSessionIdRef: MutableRefObject<string | null>;
  onPrefetchRef: MutableRefObject<
    ((info: { intent: string; confidence: number; results: PrefetchResultEntry[] }) => void) | undefined
  >;
  streamingSessionIdRef: MutableRefObject<string | null>;

  // ── component-level state setters ──
  setStreamingSessionId: Dispatch<SetStateAction<string | null>>;
  setStreamingStatus: Dispatch<SetStateAction<string | null>>;
  setLastActivityAt: Dispatch<SetStateAction<number | null>>;
  setContextUsage: Dispatch<SetStateAction<ContextUsage | null>>;
  setUsageSnapshot: Dispatch<SetStateAction<UsageSnapshot | null>>;
  setCompactionStatus: Dispatch<SetStateAction<CompactionStatus | null>>;

  // ── per-send mutable locals (shared between handlers) ──
  fullText: string;
  phase: StreamPhase;
  firstChunkReceived: boolean;
  hasContent: boolean;
  hasAnyStreamEvent: boolean;
  approvalPending: boolean;
  idleTimeout: ReturnType<typeof setTimeout> | null;
  slowHintTimer: ReturnType<typeof setTimeout> | null;
  staleStreamTimer: ReturnType<typeof setTimeout> | null;
  chunkBuffer: string;
  chunkRaf: number | null;
  retryCount: number;

  // ── P5 L3 seq 对账（设计 §3.5 / §6.3 P7）──────────────────────────
  // 本 attachment 的本地 seq 水位：每收到一帧带可选 `seq` 字段的
  // WsEnvelope（`data.seq`，控制帧为 null），把 max 记在这里。
  // 重连对账（useStreamChat 的 onConnectionLost 路径）用它作
  // `subscribeSessionSnapshot` 的 since_seq——已追上（服务端
  // lastSeq <= 本水位）回空增量刷新锚；落后回全量快照整体替换。
  // 只增不减（max 语义），跨 send 不重置（per-attachment 持久水位）。
  lastLocalSeq: number;

  // ── helper closures wired from `send` ──
  updatePhase: (next: StreamPhase) => void;
  getPhase: () => StreamPhase;
  settle: (
    text: string,
    err?: string,
    reason?: string,
    phaseHint?: StreamPhase,
    detail?: string,
    keepSessionAlive?: boolean,
    kind?: string,
  ) => void;
  armTimeouts: () => void;
  disarmTimeouts: () => void;
  resetIdleTimeout: () => void;
  resetStaleTimer: () => void;
  resetMidStreamSilenceTimer: () => void;
  scheduleChunkFlush: () => void;
  wsSendRaw: (text: string) => boolean;
  /** Send this turn's user message upstream. Idempotent — the second
   *  call on the same send is a no-op, because both the `connected`
   *  branch and the "listener registration completed" path may try to
   *  dispatch it. */
  dispatchUserMessage: () => void;
}

/**
 * Build the `WebSocket.onmessage` handler as a standalone function.
 *
 * This is a verbatim lift of the original `ws.onmessage` body from
 * `useStreamChat.ts` — every branch (text / thinking / tool_* /
 * file_change / memory_* / todo_* / status / request_approval /
 * clarify_request / acui / context_usage / usage_update /
 * compaction_status / section_gate / done / error / prefetch_intent)
 * is preserved byte-for-byte in behaviour; only the closure-local
 * variables were renamed to their `ctx.*` equivalents.
 */
export function createMessageHandler(
  ctx: StreamChatSendContext,
): (payloadText: string) => void {
  // Reset the IDLE timer (300s). Called on every received bridge
  // frame. If no frame arrives within IDLE_TIMEOUT_MS, the stream
  // is considered abandoned and we settle with "timeout". Mirrors
  // the same constant used by `resetIdleTimeout` in `send`.
  const IDLE_TIMEOUT_MS = 300_000;

  return (payloadText: string) => {
    // Stale WS guard — if a new send has started, ignore events from the old WS.
    if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
    // Reset the idle timeout on every received event. As long as the agent is
    // actively producing data (text, thinking, tool events, status), the stream
    // stays alive. Only when no event arrives for IDLE_TIMEOUT_MS does the
    // timeout fire.
    // Skip the idle reset while an approval is pending — the agent is blocked
    // waiting for the user's click, so no WS events arrive and the timer would
    // otherwise fire after 300s and kill the wait.
    if (ctx.approvalPending) {
      // Still clear any existing timer so it doesn't fire from before the
      // approval was pending.
      if (ctx.idleTimeout !== null) {
        clearTimeout(ctx.idleTimeout);
        ctx.idleTimeout = null;
      }
    } else if (ctx.idleTimeout !== null) {
      clearTimeout(ctx.idleTimeout);
      ctx.idleTimeout = setTimeout(() => {
        if (!ctx.doneRef.current) {
          ctx.updatePhase("timeout");
          ctx.settle("", undefined, "timeout", "timeout");
        }
      }, IDLE_TIMEOUT_MS);
    }
    // Arm the stale timer on every WS event. Skip while an approval is pending
    // — same reasoning as the idle guard above.
    if (!ctx.approvalPending) {
      ctx.resetStaleTimer();
      ctx.resetMidStreamSilenceTimer();
    }
    try {
      const data: any = JSON.parse(payloadText);

      // P5 L3 seq 对账：可选 seq 字段（WsEnvelope.seq，控制帧为
      // null/undefined——`Connected`/`Status`/合成 `Done` 无对账语义，
      // 只有广播镜像点取号的被镜像事件帧嵌 seq）。max 语义只增
      // 不减；没有 seq 的帧跳过（不重置水位）。重连对账时用
      // `ctx.lastLocalSeq` 作 subscribe 的 since_seq。
      const frameSeq =
        typeof data?.seq === "number"
          ? data.seq
          : typeof data?.event?.seq === "number"
            ? data.event.seq
            : null;
      if (frameSeq !== null && frameSeq > ctx.lastLocalSeq) {
        ctx.lastLocalSeq = frameSeq;
      }

      // Connected event — the transport is (re)established. The user
      // message is dispatched via `ctx.dispatchUserMessage()` rather
      // than inline, because on the stdio bridge this event is NOT
      // guaranteed to arrive for every send (see the connector: a
      // channel is created once per session and survives across
      // sends, so only the first send on a session ever sees
      // `connected`). `dispatchUserMessage` is idempotent, so
      // whichever of the two paths fires first wins and the other is
      // a no-op.
      if (data?.type === "connected" || data?.event?.type === "connected") {
        ctx.dispatchUserMessage();
        return;
      }

      // Streaming text
      const evtType = data?.type || data?.event?.type;
      const evtContent = data?.content || data?.event?.content;

      if (evtType === "text") {
        // Reset the stale-stream timer on every text chunk so tool-only turns
        // (no user-visible output) don't keep the stream alive indefinitely.
        // Also reset mid-stream silence watchdog — any event = the connection
        // is making forward progress.
        ctx.resetStaleTimer();
        ctx.resetMidStreamSilenceTimer();
        ctx.firstChunkReceived = true;
        if (ctx.getPhase() === "slow") ctx.updatePhase("streaming");
        const piece = typeof evtContent === "string" ? evtContent : "";
        // Defense-in-depth: if the old backend still injects
        // [activity: …] / [status: …] as Text events (before the lib.rs fix
        // was compiled), silently drop them so they never pollute message body.
        if (piece && !/^\[(?:activity|status):\s*/.test(piece)) {
          ctx.hasContent = true;
          ctx.hasAnyStreamEvent = true;
          ctx.setLastActivityAt(Date.now());
          ctx.fullText += piece;
          ctx.chunkBuffer += piece;
          ctx.scheduleChunkFlush();
        }
        return;
      }

      // Live reasoning text (server emits `StreamEvent::Thinking` chunks as
      // the LLM produces its `<think>…</think>` monologue). Forward raw to
      // caller — caller accumulates into `message.reasoningContent`. We
      // intentionally do NOT merge into `fullText` (the visible reply) and do
      // NOT count toward `hasContent` (the done-event check).
      if (evtType === "thinking") {
        const piece = typeof evtContent === "string" ? evtContent : "";
        if (piece) {
          ctx.hasAnyStreamEvent = true;
          ctx.setLastActivityAt(Date.now());
          ctx.callbacks.onReasoningChunk?.(piece);
        }
        return;
      }

      if (evtType === "tool_start") {
        const tool = data?.tool || data?.event?.tool || "tool";
        const toolCallId =
          data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
        ctx.toolRunningRef.current = true;
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        ctx.callbacks.onToolStart?.({ toolCallId, tool, content: evtContent });
        return;
      }

      if (evtType === "tool_complete") {
        const tool = data?.tool || data?.event?.tool || "tool";
        const toolCallId =
          data?.tool_call_id || data?.event?.tool_call_id || `legacy-${tool}`;
        // (A+C product fix): a user-facing denial (user拒绝 / 超时 / 连接中断)
        // carries `denial_kind`. Forward it so App.tsx can show a "this call
        // was blocked" banner that matches the agent's injected "tell the
        // user" note. `"policy"` is a silent gate and is intentionally ignored
        // here.
        const denialKind =
          data?.denial_kind || data?.event?.denial_kind || undefined;
        // Server-authoritative failure flag (`ToolResult::is_error`).
        // `undefined` means a server build that predates the flag.
        const rawIsError = data?.is_error ?? data?.event?.is_error;
        const isError =
          typeof rawIsError === "boolean" ? rawIsError : undefined;
        // Flip toolRunningRef back to false BEFORE resetStaleTimer so the
        // watchdog gets re-armed for a fresh STALE window.
        ctx.toolRunningRef.current = false;
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        ctx.resetStaleTimer();
        ctx.resetMidStreamSilenceTimer();
        ctx.callbacks.onToolComplete?.({ toolCallId, tool, content: evtContent, denialKind, isError });
        return;
      }

      if (evtType === "sub_agent_event") {
        // Live sub-agent lifecycle event. The server serialises it as
        // `WsEnvelope.event` with `type: "sub_agent_event"` and the fields on
        // `data.event`. Forward to the caller so the "子代理" panel can update
        // the matching row.
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data;
        ctx.callbacks.onSubAgentEvent?.({
          subAgentId: ev?.sub_agent_id,
          phase: ev?.phase,
          task: typeof ev?.task === "string" ? ev.task : "",
          role: ev?.role,
          turns: typeof ev?.turns === "number" ? ev.turns : undefined,
          costUsd: typeof ev?.cost_usd === "number" ? ev.cost_usd : undefined,
          error: ev?.error,
          // Rolling step log (tool calls / thinking), newest last. Mirrors the
          // backend `recent_steps` array. May be absent on older servers.
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
          ctx.hasAnyStreamEvent = true;
          ctx.setLastActivityAt(Date.now());
          ctx.callbacks.onToolChunk?.({
            toolCallId,
            tool,
            content,
            chunkIndex,
            chunkTotal,
          });
        }
        return;
      }

      // File-change summary (WorkBuddy-style "文件变更" row). Server emits this
      // after a successful write_file / patch. We pull fields from either the
      // top-level payload or `event.*` to be tolerant of the two WS envelope
      // shapes.
      if (evtType === "file_change") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
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
        ctx.callbacks.onFileChange?.({
          toolCallId,
          filePath,
          changeType,
          fileSummary,
        });
        return;
      }

      // Memory mutation surfaced inline (WorkBuddy-style "记忆" row). Mirrors
      // the `file_change` branch shape.
      if (evtType === "memory_update") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data ?? {};
        const action = (ev?.action ?? ev?.memory_action ?? "write") as
          | "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
        const memoryType = (ev?.memory_type ?? ev?.memoryType ?? "fact") as
          | "fact" | "style" | "summary" | "tool" | "snapshot" | "reflection";
        const key = ev?.key ?? ev?.memory_key ?? "";
        const summary = ev?.summary ?? ev?.memory_summary ?? "";
        const count = ev?.count ?? ev?.memory_count ?? undefined;
        if (!summary && !key) return;
        ctx.callbacks.onMemoryUpdate?.({
          action,
          memoryType,
          key,
          summary,
          count,
        });
        return;
      }

      // recall event: fires when the agent re-pulls related facts from memory
      // this turn. Forward to onMemoryRecall so App.tsx can render a small
      // "💭 想到 N 条" chip in the conversation.
      if (evtType === "memory_recall") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
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
        ctx.callbacks.onMemoryRecall?.({
          trigger,
          count,
          totalChars,
          previewKeys,
          turnNumber,
        });
        return;
      }

      // Todo (task list) mutations. Routing is straight to onTodoChange.
      if (evtType === "todo_snapshot") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data ?? {};
        const items = ev?.items ?? [];
        const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
        if (Array.isArray(items)) {
          ctx.callbacks.onTodoChange?.({
            kind: "snapshot",
            sessionId,
            items: items as import("./useStreamChat").TodoSnapshotItem[],
          });
        }
        return;
      }
      if (evtType === "todo_group_started") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data ?? {};
        const groupId = ev?.group_id ?? ev?.groupId ?? "";
        const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
        const groupTitle = ev?.group_title ?? ev?.groupTitle ?? null;
        const messageId = ev?.message_id ?? ev?.messageId ?? null;
        if (groupId) {
          ctx.callbacks.onTodoChange?.({
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
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data ?? {};
        const item = ev?.item;
        const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
        if (item && item.id) {
          ctx.callbacks.onTodoChange?.({
            kind: "upsert",
            sessionId,
            item: item as import("./useStreamChat").TodoSnapshotItem,
          });
        }
        return;
      }
      if (evtType === "todo_item_removed") {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data ?? {};
        const id = ev?.id ?? "";
        const sessionId = ev?.session_id ?? ev?.sessionId ?? "";
        if (id) {
          ctx.callbacks.onTodoChange?.({ kind: "remove", sessionId, id });
        }
        return;
      }

      // Backend status / activity notifications (e.g. "Still waiting for the
      // model…"). Displayed as a chip in StreamingBubble, NOT injected into
      // message text.
      if (evtType === "status" || evtType === "activity") {
        const content =
          typeof evtContent === "string" ? evtContent : "";
        ctx.setStreamingStatus(content || null);
        // `task_in_flight` on a Status means the backend consumed a user
        // message that arrived mid-stream and QUEUED it — the turn for it
        // runs as soon as the current one finishes, and its output lands on
        // this same send. So this is NOT terminal: settling here would end
        // the bubble before the queued turn ever produced anything, and the
        // real answer would have nowhere to render.
        //
        // Keep the send promise alive and re-arm the timers, so that if the
        // queued turn never materialises (backend restarted, agent died) the
        // idle/stale path still settles the bubble instead of hanging it
        // forever. The Status text is already shown as the chip above, so the
        // user knows their message was queued.
        if (content.includes('"task_in_flight"')) {
          ctx.armTimeouts();
          return;
        }
        return;
      }

      // Server is asking for explicit user approval before running a high-risk
      // tool. Forward to caller; when they answer, send the response back via
      // the same socket.
      if (evtType === "request_approval" || evtType === "RequestApproval") {
        // Suspend idle/stale timers while the approval card is shown: the agent
        // loop is blocked waiting for the user's click, so no WS events arrive
        // and the stream would otherwise settle as "timeout" after 300s and
        // kill the approval wait. Timers are re-armed in the .then()/.catch().
        ctx.approvalPending = true;
        if (ctx.idleTimeout !== null) { clearTimeout(ctx.idleTimeout); ctx.idleTimeout = null; }
        if (ctx.staleStreamTimer !== null) { clearTimeout(ctx.staleStreamTimer); ctx.staleStreamTimer = null; }
        // The agent loop is paused waiting for the user's click, but the turn's
        // `done` event only arrives after the approval resolves — so
        // `streamingSessionId` would otherwise stay set and ChatView keeps
        // rendering a "正在思考…" bubble next to the approval card. The approval
        // card itself IS the waiting state; clear the streaming placeholder now.
        ctx.streamingSessionIdRef.current = null;
        ctx.setStreamingSessionId(null);
        ctx.setStreamingStatus(null);
        const ev = data?.event ?? data;
        const requestId = ev?.request_id ?? ev?.requestId;
        const tool = ev?.tool ?? "tool";
        const args = ev?.args ?? ev?.arguments;
        const reason = typeof ev?.reason === "string" ? ev.reason : undefined;
        // The server's `StreamEvent::RequestApproval` field is `risk`
        // (snake_case in JSON), but older drafts used `risk_level` / `riskLevel`.
        const riskLevel = ev?.risk_level ?? ev?.riskLevel ?? ev?.risk;
        // `expires_at` is Unix milliseconds. Older servers may omit it.
        const expiresAt: number | null =
          typeof ev?.expires_at === "number" ? ev.expires_at : null;
        if (!requestId) return;
        if (ctx.callbacks.onApprovalRequest) {
          ctx.callbacks
            .onApprovalRequest({ requestId, tool, args, riskLevel, reason, expiresAt, sessionId: ctx.sessionId })
            .then((resp) => {
              // Re-arm idle/stale timers — the agent loop is about to resume.
              ctx.approvalPending = false;
              ctx.armTimeouts();
              // The `expired` decision means the server already settled this
              // request; do NOT send `approval_response` over the wire.
              if (resp.decision === "expired") return;
              // Restore the streaming placeholder cleared when the card
              // opened. There is a ~1s gap between the user's click and the
              // agent loop's next WS event; without this the chat shows dead
              // whitespace where the waiting card used to be.
              ctx.streamingSessionIdRef.current = ctx.sessionId;
              ctx.setStreamingSessionId(ctx.sessionId);
              // Always use `wsRef.current` (the live WS); the closure-captured
              // `ws` may have been replaced by a reconnect by the time the user
              // clicks.
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) {
                console.warn(
                  `[approval] no active stream subscription — click for ${requestId} dropped; user must retry`,
                );
                return;
              }
              try {
                // Wire decisions: legacy two-state `approved`/`denied`
                // (server maps them to Once/Deny) plus grant persistence —
                // 「以后自动允许」sends `always`, which the server stores
                // into the durable allowlist for identical future calls.
                const decision =
                  resp.remember === true
                    ? "always"
                    : resp.decision === "approved"
                      ? "approved"
                      : "denied";
                const payload = JSON.stringify({
                  type: "approval_response",
                  request_id: requestId,
                  decision,
                });
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload,
                });
              } catch (e) {
                console.warn(
                  `[approval] backend_channel_send threw for ${requestId}: ${String(e)}`,
                );
              }
            })
            .catch(() => {
              // Re-arm idle/stale timers.
              ctx.approvalPending = false;
              ctx.armTimeouts();
              // Denial also resumes the agent loop — restore the streaming
              // placeholder here too (same dead-air rationale as above).
              ctx.streamingSessionIdRef.current = ctx.sessionId;
              ctx.setStreamingSessionId(ctx.sessionId);
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) return;
              try {
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "approval_response",
                    request_id: requestId,
                    decision: "denied",
                  }),
                });
              } catch {}
            });
        }
        return;
      }

      // Batch approval: server coalesced N parallel approvals into one
      // envelope (see `coalescer::ApprovalCoalescer`). Single decision
      // applies to every item; we mirror the single-item branch's
      // streaming-placeholder restoration + WS send.
      if (evtType === "request_approvals_batch" || evtType === "RequestApprovalsBatch") {
        ctx.approvalPending = true;
        if (ctx.idleTimeout !== null) { clearTimeout(ctx.idleTimeout); ctx.idleTimeout = null; }
        if (ctx.staleStreamTimer !== null) { clearTimeout(ctx.staleStreamTimer); ctx.staleStreamTimer = null; }
        ctx.streamingSessionIdRef.current = null;
        ctx.setStreamingSessionId(null);
        ctx.setStreamingStatus(null);
        const ev = data?.event ?? data;
        const batchId = ev?.batch_id ?? ev?.batchId;
        const rawItems = Array.isArray(ev?.items) ? ev.items : [];
        if (!batchId || rawItems.length === 0) return;
        const items = rawItems.map((it: Record<string, unknown>) => ({
          requestId: (it?.request_id ?? it?.requestId ?? "") as string,
          tool: (it?.tool ?? "tool") as string,
          args: it?.arguments ?? it?.args,
          risk: (it?.risk ?? undefined) as string | undefined,
          reason: typeof it?.reason === "string" ? (it.reason as string) : null,
          expiresAt: typeof it?.expires_at === "number" ? (it.expires_at as number) : null,
          // Backend pre-computed unified diff for file-mutating items;
          // non-file tools (run_command / http / …) leave it absent.
          diff: typeof it?.diff === "string" ? (it.diff as string) : null,
        }));
        if (ctx.callbacks.onApprovalBatchRequest) {
          ctx.callbacks
            .onApprovalBatchRequest({ batchId, items, sessionId: ctx.sessionId })
            .then((resp) => {
              ctx.approvalPending = false;
              ctx.armTimeouts();
              if (resp.decision === "expired") return;
              ctx.streamingSessionIdRef.current = ctx.sessionId;
              ctx.setStreamingSessionId(ctx.sessionId);
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) {
                console.warn(
                  `[approval-batch] no active stream subscription — click for ${batchId} dropped; user must retry`,
                );
                return;
              }
              try {
                const decision =
                  resp.remember === true
                    ? "always"
                    : resp.decision === "approved"
                      ? "approved"
                      : "denied";
                const payload = JSON.stringify({
                  type: "approval_responses_batch",
                  batch_id: batchId,
                  decision,
                });
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload,
                });
              } catch (e) {
                console.warn(
                  `[approval-batch] backend_channel_send threw for ${batchId}: ${String(e)}`,
                );
              }
            })
            .catch(() => {
              ctx.approvalPending = false;
              ctx.armTimeouts();
              ctx.streamingSessionIdRef.current = ctx.sessionId;
              ctx.setStreamingSessionId(ctx.sessionId);
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) return;
              try {
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "approval_responses_batch",
                    batch_id: batchId,
                    decision: "denied",
                  }),
                });
              } catch {}
            });
        }
        return;
      }

      // Server is asking the user a clarification question. Render a question
      // card; when the user picks / types an answer, send it back as
      // `clarify_response`.
      if (evtType === "clarify_request" || evtType === "ClarifyRequest") {
        const ev = data?.event ?? data;
        const requestId: string | undefined =
          ev?.request_id ?? ev?.requestId;
        const question: string = ev?.question ?? "";
        const choices: string[] = Array.isArray(ev?.choices) ? ev.choices : [];
        if (!requestId) return;
        if (ctx.callbacks.onClarifyRequest) {
          ctx.callbacks
            .onClarifyRequest({ requestId, question, choices, sessionId: ctx.sessionId })
            .then((resp) => {
              // stale-click hardening — use wsRef.current so a reconnect between
              // `clarify_request` arrival and the user's answer doesn't silently
              // drop the reply.
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) return;
              try {
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "clarify_response",
                    request_id: requestId,
                    answer: resp.answer ?? "",
                  }),
                });
              } catch {}
            })
            .catch(() => {
              const liveUnlisten = ctx.wsRef.current;
              if (!liveUnlisten) return;
              try {
                void invoke<void>("backend_channel_send", {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "clarify_response",
                    request_id: requestId,
                    answer: "",
                  }),
                });
              } catch {}
            });
        }
        return;
      }

      // M5: ACUI directive (agent → UI structured card push).
      if (evtType === "acui" || evtType === "Acui") {
        const directive = data?.directive ?? data?.event?.directive;
        if (directive && typeof directive === "object" && "op" in directive) {
          ctx.callbacks.onAcuiDirective?.(directive as AcuiDirective);
        }
        return;
      }

      // Context usage snapshot — fired after every agent-loop turn.
      if (evtType === "context_usage" || evtType === "ContextUsage") {
        // Drop events whose WS session != the active session, so a background
        // session's context_usage can't pollute the bar of the session the user
        // switched to.
        if (ctx.activeSessionIdRef.current && ctx.wsSessionIdRef.current
            && ctx.activeSessionIdRef.current !== ctx.wsSessionIdRef.current) {
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
        ctx.setContextUsage({ usedTokens: used, maxTokens: max, thresholdTokens: threshold, percent: pct });
        return;
      }

      // per-turn usage snapshot — backend fires this after every successful LLM
      // response so the chat header can show a live "cache hit %" chip.
      if (evtType === "usage_update" || evtType === "UsageUpdate") {
        // same session filter as context_usage above.
        if (ctx.activeSessionIdRef.current && ctx.wsSessionIdRef.current
            && ctx.activeSessionIdRef.current !== ctx.wsSessionIdRef.current) {
          return;
        }
        const ev = data?.event ?? data ?? {};
        const toNum = (v: unknown): number =>
          typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : 0;
        ctx.setUsageSnapshot({
          promptTokens: toNum(ev.prompt_tokens),
          completionTokens: toNum(ev.completion_tokens),
          cacheReadTokens: toNum(ev.cache_read_tokens),
          cacheCreationTokens: toNum(ev.cache_creation_tokens),
          reasoningTokens: toNum(ev.reasoning_tokens),
        });
        return;
      }

      // compaction-pipeline status snapshot forwarded from the backend's
      // `BreakerSnapshot`.
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
          const maxTokens = toNumber(snap.max_tokens);
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
          const rawPhase = typeof snap.phase === "string" ? snap.phase : "idle";
          const phase: import("../components/CompactionStatusBanner").CompactionPhase =
            rawPhase === "starting" ||
            rawPhase === "in_progress_fast" ||
            rawPhase === "in_progress_llm" ||
            rawPhase === "complete" ||
            rawPhase === "failed"
              ? rawPhase
              : "idle";
          ctx.setCompactionStatus({
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

      // (C3 marker protocol): the backend's `SectionGate` forwards the
      // resulting `SectionGateReport` to the `on_section_gate` callback.
      if (evtType === "section_gate" || evtType === "SectionGate") {
        const narrowed = narrowSectionGateReport(
          data?.report ?? data?.event?.report ?? null,
        );
        if (narrowed) {
          ctx.callbacks.onSectionGate?.(narrowed);
        }
        return;
      }

      if (evtType === "done") {
        const reason: string | undefined =
          data?.finish_reason || data?.event?.finish_reason;
        // Terminal outcome classification from the server.
        const outcome: string | undefined =
          data?.outcome || data?.event?.outcome;
        const isFailedOutcome =
          outcome === "failed" || outcome === "interrupted";
        const rawError: string | undefined =
          data?.error || data?.event?.error;
        if (isFailedOutcome || reason === "error" || !!rawError) {
          const errMsg = rawError ?? "对话因后端错误中断（已保留已生成的部分内容，请稍后重试）";
          const errKind =
            (typeof data?.code === "string" && data.code) ||
            (typeof data?.event?.code === "string" && data.event.code) ||
            (rawError ? "agent_error" : undefined);
          ctx.settle("", errMsg, undefined, "error", rawError, true, errKind);
          return;
        }
        // M3 may send a `done` with empty `evtContent`. Always prefer
        // `fullText` (the accumulated text from all `text` events).
        const text = (typeof evtContent === "string" && evtContent.length > 0)
          ? evtContent
          : ctx.fullText;
        // A `deduped` / `task_in_flight` Done carries no content by design —
        // it is a protocol-level "this send was coalesced, the real answer
        // is elsewhere" marker. Settling with an empty string renders as
        // "the bubble vanished and no reply appeared", which reads as a
        // broken app. Give the user an explicit, honest line instead.
        const resolvedText =
          text.length > 0
            ? text
            : reason === "deduped"
              ? "上一条相同的消息刚刚已经发送过，无需重复发送。"
              : reason === "task_in_flight"
                ? "此会话已有正在运行的任务，你的新消息没有触发新一轮处理。请等待当前任务完成，或点击「停止」后再发送。"
                : text;
        const isWatchdogOrTimeout =
          reason === "turn_transition_stall" ||
          reason === "no_first_chunk" ||
          reason === "stream_overall_timeout";
        const phase: StreamPhase = isWatchdogOrTimeout
          ? "timeout"
          : "completed";
        ctx.settle(
          resolvedText,
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
        ctx.settle("", errMsg, undefined, "error", errDetail, undefined, errKind);
        return;
      }

      // H3: prefetch intent event — prefetcher resolved with a user-intent
      // prediction and speculatively ran safe tools.
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
        ctx.onPrefetchRef.current?.({ intent, confidence, results });
        return;
      }
    } catch {
      // ignore malformed frames
    }
  };
}
